/*
 *  Copyright (C) 2024 Michael Bachmann
 *
 *  This program is free software: you can redistribute it and/or modify
 *  it under the terms of the GNU Affero General Public License as published by
 *  the Free Software Foundation, either version 3 of the License, or
 *  (at your option) any later version.
 *
 *  This program is distributed in the hope that it will be useful,
 *  but WITHOUT ANY WARRANTY; without even the implied warranty of
 *  MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 *  GNU Affero General Public License for more details.
 *
 *  You should have received a copy of the GNU Affero General Public License
 *  along with this program.  If not, see <https://www.gnu.org/licenses/>.
 */

use super::{
    audio_system::{AudioSystem, JackAudioSystem},
    socket::{create_ipv4_rx_socket, create_ipv6_rx_socket},
    Matrix,
};
use crate::{
    actor::{respond, Actor, ActorApi},
    error::{RxError, RxResult},
    status::{Receiver, Status, StatusApi},
    utils::{init_buffer, RtpSequenceBuffer},
    ReceiverId,
};
use core::f32;
use sdp::{
    description::common::{Address, ConnectionInformation},
    SessionDescription,
};
use std::{collections::HashMap, net::IpAddr};
use tokio::{
    net::UdpSocket,
    select, spawn,
    sync::{
        mpsc::{self},
        oneshot,
    },
};
use tokio_graceful_shutdown::{SubsystemBuilder, SubsystemHandle};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone)]
pub struct RxDescriptor {
    pub id: usize,
    pub session_name: String,
    pub session_id: u64,
    pub session_version: u64,
    pub bit_depth: usize,
    pub channels: usize,
    pub sampling_rate: usize,
    pub packet_time: f32,
    pub link_offset: f32,
    pub origin_ip: IpAddr,
}

impl RxDescriptor {
    fn new(receiver_id: usize, sd: &SessionDescription, link_offset: f32) -> Result<Self, RxError> {
        let media = if let Some(it) = sd.media_descriptions.iter().next() {
            it
        } else {
            return Err(RxError::InvalidSdp("no media description found".to_owned()));
        };

        let fmt = if let Some(format) = media.media_name.formats.iter().next() {
            format
        } else {
            return Err(RxError::InvalidSdp("no media format found".to_owned()));
        };

        // TODO make sure the right rtpmap is picked in case there is more than one
        let rtpmap = if let Some(Some(it)) = media.attribute("rtpmap") {
            it
        } else {
            return Err(RxError::InvalidSdp("no rtpmap found".to_owned()));
        };

        if !rtpmap.starts_with(fmt) {
            return Err(RxError::InvalidSdp(
                "rtpmap and media description payload types do not match".to_owned(),
            ));
        }

        let stream_format = rtpmap.replace(fmt, "").trim().to_owned();

        let mut split = stream_format.split('/');
        let (bit_depth, sampling_rate, channels): (usize, usize, usize) =
            if let (Some(bit_depth), Some(sampling_rate), Some(channels)) = (
                split.next().and_then(|it| it[1..].parse().ok()),
                split.next().and_then(|it| it.parse().ok()),
                split.next().and_then(|it| it.parse().ok()),
            ) {
                (bit_depth, sampling_rate, channels)
            } else {
                return Err(RxError::InvalidSdp(
                    "could not get bit depth, sampling rate and channels from rtpmap".to_owned(),
                ));
            };

        let packet_time = if let Some(ptime) = media
            .attribute("ptime")
            .and_then(|it| it)
            .and_then(|p| p.parse().ok())
        {
            ptime
        } else {
            return Err(RxError::InvalidSdp("no ptime".to_owned()));
        };

        let session_name = sd.session_name.clone();
        let session_id = sd.origin.session_id;
        let session_version = sd.origin.session_version;
        let origin_ip = sd
            .origin
            .unicast_address
            .parse()
            .map_err(|e| RxError::Other(format!("error parsing origin IP: {e}")))?;

        Ok(RxDescriptor {
            id: receiver_id,
            session_name,
            session_id,
            session_version,
            bit_depth,
            channels,
            sampling_rate,
            packet_time,
            link_offset,
            origin_ip,
        })
    }

    pub fn session_id_from_sdp(sdp: &SessionDescription) -> String {
        format!("{} {}", sdp.origin.session_id, sdp.origin.session_version)
    }

    pub fn session_id(&self) -> String {
        format!("{} {}", self.session_id, self.session_version)
    }

    pub fn rtp_header_len() -> usize {
        12
    }

    pub fn max_samplerate() -> usize {
        96000
    }

    pub fn max_bit_depth() -> usize {
        32
    }

    pub fn max_packet_time() -> usize {
        4
    }

    pub fn max_packet_size(channels: usize) -> usize {
        Self::rtp_header_len()
            * (channels * Self::max_samplerate() * Self::max_packet_time() * Self::max_bit_depth())
            / 8
            / 1000
    }

    pub fn bytes_per_sample(&self) -> usize {
        self.bit_depth / 8
    }

    pub fn bytes_per_frame(&self) -> usize {
        self.channels * self.bytes_per_sample()
    }

    pub fn frames_per_packet(&self) -> usize {
        f32::ceil(self.sampling_rate as f32 * self.packet_time / 1000.0) as usize
    }

    pub fn samples_per_packet(&self) -> usize {
        self.channels * self.frames_per_packet()
    }

    pub fn packets_in_link_offset(&self) -> usize {
        f32::ceil(self.link_offset / self.packet_time) as usize
    }

    pub fn frames_per_link_offset_buffer(&self) -> usize {
        self.packets_in_link_offset() * self.frames_per_packet()
    }

    pub fn rtp_payload_size(&self) -> usize {
        self.frames_per_packet() * self.bytes_per_frame()
    }

    pub fn rtp_packet_size(&self) -> usize {
        Self::rtp_header_len() + self.rtp_payload_size()
    }

    pub fn audio_buffer_size(&self) -> usize {
        self.channels * self.frames_per_link_offset_buffer()
    }

    pub fn rtp_buffer_size(&self) -> usize {
        self.packets_in_link_offset() * self.rtp_packet_size()
    }

    pub fn to_link_offset(&self, samples: usize) -> usize {
        f32::ceil(samples as f32 / (self.sampling_rate as f32 / 1000.0)) as usize
    }
}

// #[derive(Debug)]
// enum Dealloc {
//     Socket(Socket),
//     RxDescriptor(RxDescriptor),
// }

// #[derive(Debug)]
// enum RxThreadEvent {
//     BufferOverflow(ReceiverId),
//     BufferUnderflow(ReceiverId),
//     ThreadStopped,
//     Err(RxError),
//     Dealloc(Dealloc),
// }
#[derive(Debug)]
pub enum RxThreadFunction {
    StartReceiver(ReceiverId, RxDescriptor, UdpSocket, Response<()>),
    StopReceiver(ReceiverId, Response<()>),
}

// struct RxThreadDestructor {
//     event_tx: mpsc::UnboundedSender<RxThreadEvent>,
// }

// impl Drop for RxThreadDestructor {
//     fn drop(&mut self) {
//         self.event_tx.send(RxThreadEvent::ThreadStopped).ok();
//     }
// }

#[derive(Clone)]
pub struct RtpRxApi {
    channel: mpsc::Sender<RxFunction>,
}

impl ActorApi for RtpRxApi {
    type Message = RxFunction;
    type Error = RxError;

    fn message_tx(&self) -> &mpsc::Sender<RxFunction> {
        &self.channel
    }
}

impl RtpRxApi {
    pub fn new(
        subsys: &SubsystemHandle,
        max_channels: usize,
        link_offset: f32,
        status: StatusApi,
    ) -> Result<Self, RxError> {
        let (channel, commands) = mpsc::channel(1);

        let (trans_init_tx, _trans_init_rx) = mpsc::channel(max_channels);
        let (recv_init_tx, recv_init_rx) = mpsc::channel(max_channels);
        let audio_system =
            JackAudioSystem::new(&subsys, 0, trans_init_tx, max_channels, recv_init_tx)
                .expect("audio system failed");

        let sample_reader = read_f32_sample;

        subsys.start(SubsystemBuilder::new("rtp/rx", move |s| async move {
            let cancel = s.create_cancellation_token();
            let mut actor: RtpRxActor<JackAudioSystem, f32> = RtpRxActor::new(
                s,
                commands,
                max_channels,
                link_offset,
                status,
                recv_init_rx,
                audio_system,
                sample_reader,
            );
            actor.log_channel_consumption().await?;
            let res = actor.run("rtp-rx".to_owned(), cancel).await;
            log::info!("RX stopped.");
            res
        }));

        Ok(RtpRxApi { channel })
    }

    pub async fn create_receiver(&self, sdp: SessionDescription) -> Result<(), RxError> {
        self.send_message(|tx| RxFunction::CreateReceiver(sdp, tx))
            .await
    }

    pub async fn delete_receiver(&self, id: ReceiverId) -> Result<(), RxError> {
        self.send_message(|tx| RxFunction::DeleteReceiver(id, tx))
            .await
    }
}

type Response<T> = oneshot::Sender<RxResult<T>>;

#[derive(Debug)]
pub enum RxFunction {
    CreateReceiver(SessionDescription, Response<()>),
    DeleteReceiver(ReceiverId, Response<()>),
}

#[derive(Debug)]
pub enum RxConfig {
    LinkOffset(usize),
}

struct RtpRxActor<AS, SampleFormat>
where
    AS: AudioSystem,
    SampleFormat: Default + Send + Sync + 'static,
{
    commands: mpsc::Receiver<RxFunction>,
    max_channels: usize,
    used_channels: usize,
    link_offset: f32,
    status: StatusApi,
    subsys: SubsystemHandle,
    recv_init_rx: mpsc::Receiver<(usize, oneshot::Sender<Box<[mpsc::Receiver<SampleFormat>]>>)>,
    _audio_system: AS,
    receiver_ids: HashMap<String, ReceiverId>,
    active_receivers: Box<[Option<(RxDescriptor, mpsc::Sender<ReceiverMessage<SampleFormat>>)>]>,
    sample_reader: fn(&[u8]) -> SampleFormat,
    matrix: Matrix,
    port_txs: Box<[Option<mpsc::Sender<SampleFormat>>]>,
}

impl<AS, S> Drop for RtpRxActor<AS, S>
where
    AS: AudioSystem,
    S: Default + Send + Sync + 'static,
{
    fn drop(&mut self) {
        self.subsys.request_shutdown();
    }
}

impl<AS, S> Actor for RtpRxActor<AS, S>
where
    AS: AudioSystem,
    S: Copy + Default + Send + Sync + 'static,
{
    type Message = RxFunction;
    type Error = RxError;

    async fn recv_message(&mut self) -> Option<RxFunction> {
        self.commands.recv().await
    }

    async fn process_message(&mut self, command: RxFunction) -> bool {
        match command {
            RxFunction::CreateReceiver(sdp, tx) => respond(self.create_receiver(sdp), tx).await,
            RxFunction::DeleteReceiver(id, tx) => respond(self.delete_receiver(id), tx).await,
        }
    }

    async fn run(&mut self, name: String, cancel_token: CancellationToken) -> RxResult<()> {
        loop {
            select! {
                _ = cancel_token.cancelled() => {
                    log::info!("Shutdown requested, stopping actor {name} …");
                    break
                },
                Some(msg) = self.commands.recv() => if !self.process_message(msg).await {
                    break;
                },
                Some((buffer_size,tx)) = self.recv_init_rx.recv() => {
                    log::info!("Initializing receiver channel buffer …");
                    tx.send(self.initialize_receiver_buffer(self.max_channels, buffer_size).await?).ok();
                },
                // Some(e) = self.rx_thread_event_rx.recv() =>  if !self.process_event(e).await {
                //     break;
                // },
                else => break,
            }
        }

        Ok(())
    }
}

impl<AS, S> RtpRxActor<AS, S>
where
    AS: AudioSystem,
    S: Copy + Default + Send + Sync + 'static,
{
    fn new(
        subsys: SubsystemHandle,
        commands: mpsc::Receiver<RxFunction>,
        max_channels: usize,
        link_offset: f32,
        status: StatusApi,
        recv_init_rx: mpsc::Receiver<(usize, oneshot::Sender<Box<[mpsc::Receiver<S>]>>)>,
        audio_system: AS,
        sample_reader: fn(&[u8]) -> S,
    ) -> Self {
        let receiver_ids = HashMap::new();
        let active_receivers = init_buffer(max_channels, |_| None);
        let matrix = Matrix::default_output(max_channels);
        let port_txs = init_buffer(max_channels, |_| None);

        RtpRxActor {
            subsys,
            commands,
            max_channels,
            used_channels: 0,
            link_offset,
            status,
            recv_init_rx,
            _audio_system: audio_system,
            receiver_ids,
            active_receivers,
            sample_reader,
            matrix,
            port_txs,
        }
    }

    // async fn process_event(&mut self, event: RxThreadEvent) -> bool {
    //     match event {
    //         RxThreadEvent::BufferOverflow(_) => todo!(),
    //         RxThreadEvent::BufferUnderflow(_) => todo!(),
    //         RxThreadEvent::ThreadStopped => false,
    //         RxThreadEvent::Err(e) => {
    //             log::error!("Error in receiver thread: {e}");
    //             false
    //         }
    //         RxThreadEvent::Dealloc(it) => {
    //             drop(it);
    //             true
    //         }
    //     }
    // }

    async fn initialize_receiver_buffer(
        &mut self,
        channels: usize,
        buffer_size: usize,
    ) -> RxResult<Box<[mpsc::Receiver<S>]>> {
        let mut senders = vec![];
        let mut receivers = vec![];

        for _ in 0..channels {
            let (tx, rx) = mpsc::channel(buffer_size);
            senders.push(Some(tx));
            receivers.push(rx);
        }

        self.port_txs = senders.into();

        for rec in &self.active_receivers {
            if let Some((desc, updates)) = rec {
                let mapping = self.get_channel_mapping(desc);
                updates
                    .send(ReceiverMessage::ChannelMapping(mapping))
                    .await
                    .ok();
            }
        }

        Ok(receivers.into())
    }

    async fn create_receiver(&mut self, sdp: SessionDescription) -> RxResult<()> {
        let session_id = RxDescriptor::session_id_from_sdp(&sdp);
        let receiver_id = if let Some(id) = self.receiver_ids.remove(&session_id) {
            self.delete_receiver(id).await?;
            id
        } else {
            if let Some(id) = self
                .active_receivers
                .iter()
                .enumerate()
                .find(|(_, e)| e.is_none())
                .map(|(i, _)| i)
            {
                id
            } else {
                return Err(RxError::MaxChannelsExceeded(self.max_channels));
            }
        };
        let desc = RxDescriptor::new(receiver_id, &sdp, self.link_offset)?;
        if self.used_channels + desc.channels > self.max_channels {
            return Err(RxError::MaxChannelsExceeded(self.max_channels));
        }
        log::info!("Creating receiver '{receiver_id}' for session '{session_id}' …",);
        self.used_channels += desc.channels;
        self.receiver_ids.insert(session_id, desc.id);

        let socket = create_rx_socket(&sdp).await?;

        let (updates_tx, updates) = mpsc::channel(1);
        let mapping = self.get_channel_mapping(&desc);
        updates_tx
            .send(ReceiverMessage::ChannelMapping(mapping))
            .await
            .ok();
        self.active_receivers[receiver_id] = Some((desc.clone(), updates_tx));

        let receive_loop = ReceiveLoop::new(desc.clone(), updates, socket, self.sample_reader);
        spawn(receive_loop.run());

        self.status
            .publish(Status::Receiver(Receiver::Created(
                desc,
                Some(sdp.marshal()),
            )))
            .await?;
        log::info!("Receiver {receiver_id} created.");
        self.log_channel_consumption().await?;

        Ok(())
    }

    fn get_channel_mapping(&self, desc: &RxDescriptor) -> Box<[Box<[mpsc::Sender<S>]>]> {
        let mut mapping = vec![vec![]; desc.channels];

        match &self.matrix {
            Matrix::Input(_, _) => panic!("RX has input matrix"),
            Matrix::Output(hash_map, _) => {
                for ch_nr in 0..desc.channels {
                    for (port, ch) in hash_map {
                        if ch.transceiver_id == desc.id && ch_nr == ch.channel_nr {
                            if let Some(tx) = &self.port_txs[*port] {
                                mapping[ch_nr].push(tx.to_owned());
                                log::info!("{ch:?} => {port}")
                            }
                        }
                    }
                }
            }
        };

        mapping
            .into_iter()
            .map(Box::from)
            .collect::<Vec<Box<[mpsc::Sender<S>]>>>()
            .into()
    }

    async fn delete_receiver(&mut self, receiver_id: ReceiverId) -> RxResult<()> {
        if receiver_id >= self.active_receivers.len() {
            return Err(RxError::InvalidReceiverId(receiver_id));
        }

        if let Some((desc, updates)) = self.active_receivers[receiver_id].take() {
            log::info!(
                "Deleting receiver '{receiver_id}' for session '{}' …",
                desc.session_id
            );

            self.receiver_ids.remove(&desc.session_id());

            updates.send(ReceiverMessage::Stop).await.ok();

            self.used_channels -= desc.channels;

            self.status
                .publish(Status::Receiver(Receiver::Deleted(desc.clone())))
                .await?;
            log::info!("Receiver {receiver_id} deleted.",);
            self.log_channel_consumption().await?;
        } else {
            return Err(RxError::InvalidReceiverId(receiver_id));
        };

        Ok(())
    }

    async fn log_channel_consumption(&self) -> RxResult<()> {
        log::info!(
            "Used up channels: {}/{}",
            self.used_channels,
            self.max_channels
        );
        self.status
            .publish(Status::UsedOutputChannels(
                self.used_channels,
                self.max_channels,
            ))
            .await?;
        Ok(())
    }
}

async fn create_rx_socket(sdp: &SessionDescription) -> Result<UdpSocket, RxError> {
    let global_c = sdp.connection_information.as_ref();

    if sdp.media_descriptions.len() > 1 {
        return Err(RxError::InvalidSdp(
            "redundant streams aren't supported yet".to_owned(),
        ));
    }

    let media = if let Some(media) = sdp.media_descriptions.iter().next() {
        media
    } else {
        return Err(RxError::InvalidSdp(
            "media description is missing".to_owned(),
        ));
    };

    if media.media_name.media != "audio" {
        return Err(RxError::InvalidSdp(format!(
            "unsupported media type: {}",
            media.media_name.media
        )));
    }

    if !(media.media_name.protos.contains(&"RTP".to_owned())
        && media.media_name.protos.contains(&"AVP".to_owned()))
    {
        return Err(RxError::InvalidSdp(format!(
            "unsupported media protocols: {:?}; only RTP/AVP is supported",
            media.media_name.protos
        )));
    }

    let port = media.media_name.port.value.to_owned() as u16;
    // let port = 0u16;

    let c = media.connection_information.as_ref().or(global_c);

    let c = if let Some(c) = c {
        c
    } else {
        return Err(RxError::InvalidSdp("connection data is missing".to_owned()));
    };

    let ConnectionInformation {
        network_type,
        address_type,
        address,
    } = c;

    let address = if let Some(address) = address {
        address
    } else {
        return Err(RxError::InvalidSdp(
            "connection-address is missing".to_owned(),
        ));
    };

    if address_type != "IP4" && address_type != "IP6" {
        return Err(RxError::InvalidSdp(format!(
            "unsupported addrtype: {}",
            address_type
        )));
    }

    if network_type != "IN" {
        return Err(RxError::InvalidSdp(format!(
            "unsupported nettype: {}",
            network_type
        )));
    }

    let Address { address, .. } = address;

    // TODO for unicast addresses check if the IP exists on this machine and reject otherwise
    // TODO for IPv4 check if the TTL allows packets to reach this machine and reject otherwise

    let mut split = address.split('/');
    let ip = split.next();
    let prefix = split.next();
    let ip_addr: IpAddr = if let (Some(ip), Some(_prefix)) = (ip, prefix) {
        ip.parse()
            .map_err(|_| RxError::InvalidSdp(format!("invalid ip address: {address}")))?
    } else {
        return Err(RxError::InvalidSdp(format!(
            "invalid ip address: {address}"
        )));
    };

    let socket = match ip_addr {
        IpAddr::V4(ipv4_addr) => create_ipv4_rx_socket(ipv4_addr, port)?,
        IpAddr::V6(ipv6_addr) => create_ipv6_rx_socket(ipv6_addr, port)?,
    };

    let tokio_socket = UdpSocket::from_std(socket.into())?;

    Ok(tokio_socket)
}

fn pad_sample(payload: &[u8]) -> [u8; 4] {
    let mut sample = [0, 0, 0, 0];
    for (i, b) in payload.iter().enumerate() {
        sample[i] = *b;
    }
    sample
}

fn read_i32_sample(payload: &[u8]) -> i32 {
    let padded = pad_sample(payload);
    i32::from_be_bytes(padded)
}

fn read_f32_sample(payload: &[u8]) -> f32 {
    let i = read_i32_sample(payload);
    (i as f64 / i32::MAX as f64) as f32
}

enum ReceiverMessage<S> {
    ChannelMapping(Box<[Box<[mpsc::Sender<S>]>]>),
    Stop,
}

struct ReceiveLoop<S, SR>
where
    SR: Fn(&[u8]) -> S,
{
    desc: RxDescriptor,
    updates: mpsc::Receiver<ReceiverMessage<S>>,
    socket: UdpSocket,
    rtp_buffer: Box<[u8]>,
    sequence_buffer: RtpSequenceBuffer,
    sample_reader: SR,
    port_transmitters: Box<[Box<[mpsc::Sender<S>]>]>,
}

impl<S, SR> ReceiveLoop<S, SR>
where
    S: Copy,
    SR: Fn(&[u8]) -> S,
{
    fn new(
        desc: RxDescriptor,
        updates: mpsc::Receiver<ReceiverMessage<S>>,
        socket: UdpSocket,
        sample_reader: SR,
    ) -> Self {
        let rtp_buffer = init_buffer(desc.rtp_packet_size(), |_| 0u8);
        let sequence_buffer = RtpSequenceBuffer::new(&desc);
        let port_transmitters = init_buffer(desc.channels, |_| vec![].into());

        Self {
            desc,
            updates,
            socket,
            sample_reader,
            rtp_buffer,
            sequence_buffer,
            port_transmitters,
        }
    }

    async fn run(mut self) {
        loop {
            select! {
                Some(update) = self.updates.recv() => if !self.apply_update(update).await {
                    break;
                },
                Ok((len, addr)) = self.socket.recv_from(&mut self.rtp_buffer) => {
                    if addr.ip() != self.desc.origin_ip {
                        // TODO fix
                        log::error!("Received packet from wrong sender: {addr}");
                    } else {
                        if let Some(audio_data) = self.sequence_buffer.update(&self.rtp_buffer[0..len]) {
                            for (i, raw_sample) in audio_data.chunks(self.desc.bytes_per_sample()).enumerate() {
                                let channel = i % self.desc.channels;
                                let port_transmitters = &self.port_transmitters[channel];
                                let sample = (self.sample_reader)(raw_sample);
                                for port_tx in port_transmitters {
                                    port_tx.send(sample).await.ok();
                                }
                            }
                        }
                    }
                },
                else => break,
            }
        }
    }

    async fn apply_update(&mut self, msg: ReceiverMessage<S>) -> bool {
        match msg {
            ReceiverMessage::ChannelMapping(mapping) => self.port_transmitters = mapping,
            ReceiverMessage::Stop => return false,
        }
        true
    }
}
