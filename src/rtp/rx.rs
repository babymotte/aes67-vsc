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
    audio_system::{AudioSystem, JackAudioSystem, Message, OutputEvent, RtpSample},
    socket::{create_ipv4_rx_socket, create_ipv6_rx_socket},
    OutputMatrix,
};
use crate::{
    actor::{respond, Actor, ActorApi},
    error::{RxError, RxResult},
    status::{Receiver, Status, StatusApi},
    utils::{self, init_buffer, RtpSequenceBuffer, SequenceBufferEvent},
    ReceiverId,
};
use core::f32;
use sdp::{
    description::common::{Address, ConnectionInformation},
    SessionDescription,
};
use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    net::UdpSocket,
    select, spawn,
    sync::{
        mpsc::{self},
        oneshot,
    },
    time::interval,
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

    pub fn bytes_per_sample(&self) -> usize {
        utils::bytes_per_sample(self.bit_depth)
    }

    pub fn bytes_per_frame(&self) -> usize {
        utils::bytes_per_frame(self.channels, self.bit_depth)
    }

    pub fn frames_per_packet(&self) -> usize {
        utils::frames_per_packet(self.sampling_rate, self.packet_time)
    }

    pub fn samples_per_packet(&self) -> usize {
        utils::samples_per_packet(self.channels, self.sampling_rate, self.packet_time)
    }

    pub fn packets_in_link_offset(&self) -> usize {
        utils::packets_in_link_offset(self.link_offset, self.packet_time)
    }

    pub fn frames_per_link_offset_buffer(&self) -> usize {
        utils::frames_per_link_offset_buffer(self.link_offset, self.packet_time, self.sampling_rate)
    }

    pub fn rtp_payload_size(&self) -> usize {
        utils::rtp_payload_size(
            self.sampling_rate,
            self.packet_time,
            self.channels,
            self.bit_depth,
        )
    }

    pub fn rtp_packet_size(&self) -> usize {
        utils::rtp_packet_size(
            self.sampling_rate,
            self.packet_time,
            self.channels,
            self.bit_depth,
        )
    }

    pub fn samples_per_link_offset_buffer(&self) -> usize {
        utils::samples_per_link_offset_buffer(
            self.channels,
            self.link_offset,
            self.packet_time,
            self.sampling_rate,
        )
    }

    pub fn rtp_buffer_size(&self) -> usize {
        utils::rtp_buffer_size(
            self.link_offset,
            self.packet_time,
            self.sampling_rate,
            self.channels,
            self.bit_depth,
        )
    }

    pub fn to_link_offset(&self, samples: usize) -> usize {
        utils::to_link_offset(samples, self.sampling_rate)
    }
}

#[derive(Debug)]
pub enum RxThreadFunction {
    StartReceiver(ReceiverId, RxDescriptor, UdpSocket, Response<()>),
    StopReceiver(ReceiverId, Response<()>),
}

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
        local_ip: Option<IpAddr>,
    ) -> Result<Self, RxError> {
        let (channel, commands) = mpsc::channel(1);

        let (trans_init_tx, _trans_init_rx) = mpsc::channel(max_channels);
        let (recv_init_tx, recv_init_rx) = mpsc::channel(max_channels);
        let (output_event_tx, output_event_rx) = mpsc::channel(1000);
        let (msg_tx, msg_rx) = mpsc::channel(1);

        let audio_system = JackAudioSystem::new(
            &subsys,
            0,
            trans_init_tx,
            max_channels,
            recv_init_tx,
            output_event_tx,
            msg_rx,
        )
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
                local_ip,
                msg_tx,
                output_event_rx,
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
    SampleFormat: Copy + Default + Send + Sync + 'static,
{
    commands: mpsc::Receiver<RxFunction>,
    max_channels: usize,
    used_channels: usize,
    link_offset: f32,
    status: StatusApi,
    subsys: SubsystemHandle,
    recv_init_rx: mpsc::Receiver<(
        usize,
        oneshot::Sender<Box<[mpsc::Receiver<RtpSample<SampleFormat>>]>>,
    )>,
    _audio_system: AS,
    receiver_ids: HashMap<String, ReceiverId>,
    active_receivers: Box<
        [Option<(
            RxDescriptor,
            mpsc::Sender<ReceiverMessage<RtpSample<SampleFormat>>>,
        )>],
    >,
    sample_reader: fn(&[u8]) -> SampleFormat,
    matrix: OutputMatrix,
    port_txs: Box<[Option<mpsc::Sender<RtpSample<SampleFormat>>>]>,
    local_ip: Option<IpAddr>,
    active_ports: Box<[bool]>,
    msg_tx: mpsc::Sender<Message>,
    output_event_rx: mpsc::Receiver<OutputEvent>,
}

impl<AS, S> Drop for RtpRxActor<AS, S>
where
    AS: AudioSystem,
    S: Copy + Default + Send + Sync + 'static,
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
                Some(output_event) = self.output_event_rx.recv() => self.process_output_event(output_event).await,
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
        recv_init_rx: mpsc::Receiver<(usize, oneshot::Sender<Box<[mpsc::Receiver<RtpSample<S>>]>>)>,
        audio_system: AS,
        sample_reader: fn(&[u8]) -> S,
        local_ip: Option<IpAddr>,
        msg_tx: mpsc::Sender<Message>,
        output_event_rx: mpsc::Receiver<OutputEvent>,
    ) -> Self {
        let receiver_ids = HashMap::new();
        let active_receivers = init_buffer(max_channels, |_| None);
        let matrix = OutputMatrix::default(max_channels);
        let port_txs = init_buffer(max_channels, |_| None);
        let active_ports = init_buffer(max_channels, |_| false);

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
            local_ip,
            active_ports,
            msg_tx,
            output_event_rx,
        }
    }

    async fn process_output_event(&self, event: OutputEvent) {
        match event {
            OutputEvent::BufferUnderrun(port) => {
                log::debug!("Buffer underrun in output {port}");
                if let Some(ch) = self.matrix.mapping.get(&port) {
                    self.status
                        .publish(Status::Receiver(Receiver::BufferUnderrun(
                            ch.transceiver_id,
                            SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .expect("your clock is seriously messed up")
                                .as_millis(),
                        )))
                        .await
                        .ok();
                }
            }
        }
    }

    async fn initialize_receiver_buffer(
        &mut self,
        channels: usize,
        buffer_size: usize,
    ) -> RxResult<Box<[mpsc::Receiver<RtpSample<S>>]>> {
        let mut senders = vec![];
        let mut receivers = vec![];

        for _ in 0..channels {
            // TODO get and use audio system sample rate
            let channel_buffer_size =
                utils::frames_per_link_offset_buffer(self.link_offset, 1.0, 48000).max(buffer_size);
            let (tx, rx) = mpsc::channel(channel_buffer_size);
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

        let socket = create_rx_socket(&sdp, self.local_ip).await?;

        let changed_ports = self
            .matrix
            .auto_route(receiver_id, desc.channels)
            .expect("port availability must be checked first");

        self.activate_ports(changed_ports).await?;

        let (updates_tx, updates) = mpsc::channel(1);
        let mapping = self.get_channel_mapping(&desc);
        updates_tx
            .send(ReceiverMessage::ChannelMapping(mapping))
            .await
            .ok();
        self.active_receivers[receiver_id] = Some((desc.clone(), updates_tx));

        let receive_loop = ReceiveLoop::new(
            desc.clone(),
            updates,
            socket,
            self.sample_reader,
            self.status.clone(),
        );
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

    fn get_channel_mapping(&self, desc: &RxDescriptor) -> Box<[Box<[mpsc::Sender<RtpSample<S>>]>]> {
        let mut mapping = vec![vec![]; desc.channels];

        for ch_nr in 0..desc.channels {
            for (port, ch) in &self.matrix.mapping {
                if ch.transceiver_id == desc.id && ch_nr == ch.channel_nr {
                    if let Some(tx) = &self.port_txs[*port] {
                        mapping[ch_nr].push(tx.to_owned());
                        log::info!("{ch:?} => {port}")
                    }
                }
            }
        }

        mapping
            .into_iter()
            .map(Box::from)
            .collect::<Vec<Box<[mpsc::Sender<RtpSample<S>>]>>>()
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

            let changed_ports = self.matrix.auto_unroute(receiver_id);

            self.deactivate_ports(changed_ports).await?;

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

    async fn activate_ports(&mut self, changed_ports: Vec<usize>) -> RxResult<()> {
        for port in changed_ports {
            self.active_ports[port] = true;
        }
        self.msg_tx
            .send(Message::ActiveOutputsChanged(self.active_ports.clone()))
            .await
            .map_err(|e| RxError::Other(format!("could not update active ports: {e}")))?;
        Ok(())
    }

    async fn deactivate_ports(&mut self, changed_ports: Vec<usize>) -> RxResult<()> {
        for port in changed_ports {
            self.active_ports[port] = false;
        }
        self.msg_tx
            .send(Message::ActiveOutputsChanged(self.active_ports.clone()))
            .await
            .map_err(|e| RxError::Other(format!("could not update active ports: {e}")))?;
        Ok(())
    }
}

async fn create_rx_socket(
    sdp: &SessionDescription,
    local_ip: Option<IpAddr>,
) -> Result<UdpSocket, RxError> {
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

    let socket = match (ip_addr, local_ip) {
        (IpAddr::V4(ipv4_addr), Some(IpAddr::V4(local_ip))) => {
            create_ipv4_rx_socket(ipv4_addr, local_ip, port)?
        }
        (IpAddr::V6(ipv6_addr), Some(IpAddr::V6(local_ip))) => {
            create_ipv6_rx_socket(ipv6_addr, local_ip, port)?
        }
        (IpAddr::V4(ipv4_addr), None) => {
            create_ipv4_rx_socket(ipv4_addr, Ipv4Addr::UNSPECIFIED, port)?
        }
        (IpAddr::V6(ipv6_addr), None) => {
            create_ipv6_rx_socket(ipv6_addr, Ipv6Addr::UNSPECIFIED, port)?
        }
        (IpAddr::V4(_), Some(IpAddr::V6(_))) => Err(RxError::Other(
            "Cannot receive IPv4 stream when bound to local IPv6 address".to_owned(),
        ))?,
        (IpAddr::V6(_), Some(IpAddr::V4(_))) => Err(RxError::Other(
            "Cannot receive IPv6 stream when bound to local IPv4 address".to_owned(),
        ))?,
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

struct ReceiveLoop<S: Copy> {
    desc: RxDescriptor,
    updates: mpsc::Receiver<ReceiverMessage<RtpSample<S>>>,
    socket: UdpSocket,
    rtp_buffer: Box<[u8]>,
    sequence_buffer: RtpSequenceBuffer<S>,
    port_transmitters: Box<[Box<[mpsc::Sender<RtpSample<S>>]>]>,
    min_buffer_usage: (usize, usize),
    status: StatusApi,
    event_rx: mpsc::UnboundedReceiver<utils::SequenceBufferEvent>,
    out_of_order_packets: usize,
    dropped_packets: usize,
}

impl<S> ReceiveLoop<S>
where
    S: Copy,
{
    fn new(
        desc: RxDescriptor,
        updates: mpsc::Receiver<ReceiverMessage<RtpSample<S>>>,
        socket: UdpSocket,
        sample_reader: fn(&[u8]) -> S,
        status: StatusApi,
    ) -> Self {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let rtp_buffer = init_buffer(desc.rtp_packet_size(), |_| 0u8);
        let sequence_buffer = RtpSequenceBuffer::new(desc.clone(), sample_reader, event_tx);
        let port_transmitters = init_buffer(desc.channels, |_| vec![].into());

        Self {
            desc,
            updates,
            socket,
            rtp_buffer,
            sequence_buffer,
            port_transmitters,
            min_buffer_usage: (0, 0),
            status,
            event_rx,
            out_of_order_packets: 0,
            dropped_packets: 0,
        }
    }

    async fn run(mut self) {
        let mut interval = interval(Duration::from_secs(1));
        loop {
            select! {
                Some(update) = self.updates.recv() => if !self.apply_update(update).await {
                    break;
                },
                Ok((len, addr)) = self.socket.recv_from(&mut self.rtp_buffer) => self.process_packet(len, addr).await,
                Some(event) = self.event_rx.recv() => self.process_event(event),
                _ = interval.tick() => self.report_statistics().await,
                else => break,
            }
        }
    }

    fn process_event(&mut self, event: SequenceBufferEvent) {
        match event {
            SequenceBufferEvent::PacketDrop => self.dropped_packets += 1,
            SequenceBufferEvent::OutOfOrderPacket => self.out_of_order_packets += 1,
        }
    }

    async fn process_packet(&mut self, len: usize, addr: SocketAddr) {
        self.update_buffer_usage();
        if addr.ip() == self.desc.origin_ip {
            let audio_data = self.sequence_buffer.update(&self.rtp_buffer[0..len]);
            for (i, sample) in audio_data.into_iter().enumerate() {
                let channel = i % self.desc.channels;
                let port_transmitters = &self.port_transmitters[channel];
                for port_tx in port_transmitters {
                    port_tx.send(*sample).await.ok();
                }
            }
            audio_data.clear();
        } else {
            log::warn!("Received packet from wrong sender: {addr}");
        }
    }

    async fn apply_update(&mut self, msg: ReceiverMessage<RtpSample<S>>) -> bool {
        match msg {
            ReceiverMessage::ChannelMapping(mapping) => {
                self.reset_buffer_usage();
                self.port_transmitters = mapping;
                self.sequence_buffer.reset();
            }
            ReceiverMessage::Stop => return false,
        }
        true
    }

    fn update_buffer_usage(&mut self) {
        let current_usage = (
            self.port_transmitters
                .iter()
                .flatten()
                .map(|s| s.max_capacity() - s.capacity())
                .min()
                .unwrap_or(0),
            self.port_transmitters
                .iter()
                .flatten()
                .map(|s| s.max_capacity())
                .min()
                .unwrap_or(0),
        );

        if current_usage.0 < self.min_buffer_usage.0 {
            self.min_buffer_usage = current_usage;
        }
    }

    async fn report_statistics(&mut self) {
        let used = self.min_buffer_usage.0;
        let available = self.min_buffer_usage.1;
        let percent = (used * 100) / available;
        log::debug!(
            "Buffer usage of receiver {}: {}/{} ({}%)",
            self.desc.id,
            used,
            available,
            percent
        );
        self.status
            .publish(Status::Receiver(Receiver::BufferUsage(
                self.desc.id,
                used,
                available,
                percent,
            )))
            .await
            .ok();
        self.reset_buffer_usage();

        self.status
            .publish(Status::Receiver(Receiver::DroppedPackets(
                self.desc.id,
                self.dropped_packets,
            )))
            .await
            .ok();
        self.dropped_packets = 0;
        self.status
            .publish(Status::Receiver(Receiver::OutOfOrderPackets(
                self.desc.id,
                self.out_of_order_packets,
            )))
            .await
            .ok();
        self.out_of_order_packets = 0;
    }

    fn reset_buffer_usage(&mut self) {
        let expected_buffer_size = self.desc.samples_per_link_offset_buffer();
        self.min_buffer_usage = (
            self.port_transmitters
                .iter()
                .flatten()
                .map(|s| s.max_capacity())
                .min()
                .unwrap_or(expected_buffer_size),
            self.port_transmitters
                .iter()
                .flatten()
                .map(|s| s.max_capacity())
                .min()
                .unwrap_or(expected_buffer_size),
        );
    }
}

#[cfg(test)]
mod test {
    use rtp_rs::Seq;
    use std::u16;

    #[test]
    fn seq_partial_ord_handles_wrap_correctly() {
        assert!(Seq::from(u16::MAX) < Seq::from(0u16))
    }
}
