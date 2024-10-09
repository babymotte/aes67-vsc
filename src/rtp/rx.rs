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
    socket::{create_ipv4_rx_socket, create_ipv6_rx_socket},
    OutputMatrix,
};
use crate::{
    actor::{respond, Actor, ActorApi},
    error::{RxError, RxResult},
    status::{Receiver, Status, StatusApi},
    utils::{self, init_buffer, AudioBuffer},
    ReceiverId, SampleFormat,
};
use core::f32;
use lazy_static::lazy_static;
use regex::Regex;
use sdp::{
    description::common::{Address, ConnectionInformation},
    SessionDescription,
};
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    time::Duration,
};
use tokio::{
    net::UdpSocket,
    select,
    sync::{
        mpsc::{self},
        oneshot,
    },
    time::interval,
};
use tokio_graceful_shutdown::{NestedSubsystem, SubsystemBuilder, SubsystemHandle};
use tokio_util::sync::CancellationToken;

lazy_static! {
    static ref MEDIA_REGEX: Regex =
        Regex::new(r"audio (.+) (.+) (.+)").expect("no dynammic input, can't fail");
    static ref RTPMAP_REGEX: Regex = Regex::new(r"([0-9]+) (.+?[0-9]+)\/([0-9]+)\/([0-9]+)")
        .expect("no dynammic input, can't fail");
    static ref TS_REFCLK_REGEX: Regex =
        Regex::new(r"ptp=(.+):(.+):(.+)").expect("no dynammic input, can't fail");
    static ref MEDIACLK_REGEX: Regex =
        Regex::new(r"direct=([0-9]+)").expect("no dynammic input, can't fail");
}
#[derive(Debug, Clone)]
pub struct RxDescriptor {
    pub id: usize,
    pub session_name: String,
    pub session_id: u64,
    pub session_version: u64,
    pub sample_format: SampleFormat,
    pub channels: usize,
    pub sample_rate: usize,
    pub packet_time: f32,
    pub link_offset: f32,
    pub origin_ip: IpAddr,
    pub rtp_offset: u32,
}

impl RxDescriptor {
    fn new(
        receiver_id: usize,
        sd: &SessionDescription,
        link_offset_len: usize,
    ) -> Result<Self, RxError> {
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

        let (payload_type, sample_format, sample_rate, channels) =
            if let Some(caps) = RTPMAP_REGEX.captures(rtpmap) {
                (
                    caps[1].to_owned(),
                    caps[2].parse()?,
                    caps[3].parse().expect("regex guarantees this is a number"),
                    caps[4].parse().expect("regex guarantees this is a number"),
                )
            } else {
                return Err(RxError::InvalidSdp("malformed rtpmap".to_owned()));
            };

        if &payload_type != fmt {
            return Err(RxError::InvalidSdp(
                "rtpmap and media description payload types do not match".to_owned(),
            ));
        }

        let packet_time = if let Some(ptime) = media
            .attribute("ptime")
            .and_then(|it| it)
            .and_then(|p| p.parse().ok())
        {
            ptime
        } else {
            return Err(RxError::InvalidSdp("no ptime".to_owned()));
        };

        let mediaclk = if let Some(it) = media.attribute("mediaclk").and_then(|it| it) {
            it
        } else {
            return Err(RxError::InvalidSdp("mediaclk".to_owned()));
        };

        let rtp_offset = if let Some(caps) = MEDIACLK_REGEX.captures(mediaclk) {
            caps[1].parse().expect("regex guarantees this is a number")
        } else {
            return Err(RxError::InvalidSdp("malformed mediaclk".to_owned()));
        };

        let session_name = sd.session_name.clone();
        let session_id = sd.origin.session_id;
        let session_version = sd.origin.session_version;
        let origin_ip = sd
            .origin
            .unicast_address
            .parse()
            .map_err(|e| RxError::Other(format!("error parsing origin IP: {e}")))?;

        let link_offset = link_offset_len as f32 / packet_time;

        Ok(RxDescriptor {
            id: receiver_id,
            session_name,
            session_id,
            session_version,
            sample_format,
            channels,
            sample_rate,
            packet_time,
            link_offset,
            origin_ip,
            rtp_offset,
        })
    }

    pub fn session_id_from_sdp(sdp: &SessionDescription) -> String {
        format!("{} {}", sdp.origin.session_id, sdp.origin.session_version)
    }

    pub fn session_id(&self) -> String {
        format!("{} {}", self.session_id, self.session_version)
    }

    pub fn bytes_per_sample(&self) -> usize {
        self.sample_format.bytes_per_sample()
    }

    pub fn bytes_per_frame(&self) -> usize {
        utils::bytes_per_frame(self.channels, self.sample_format)
    }

    pub fn frames_per_packet(&self) -> usize {
        utils::frames_per_packet(self.sample_rate, self.packet_time)
    }

    pub fn samples_per_packet(&self) -> usize {
        utils::samples_per_packet(self.channels, self.sample_rate, self.packet_time)
    }

    pub fn packets_in_link_offset(&self) -> usize {
        utils::packets_in_link_offset(self.link_offset, self.packet_time)
    }

    pub fn frames_per_link_offset_buffer(&self) -> usize {
        utils::frames_per_link_offset_buffer(self.link_offset, self.sample_rate)
    }

    pub fn link_offset_buffer_size(&self) -> usize {
        utils::link_offset_buffer_size(
            self.channels,
            self.link_offset,
            self.sample_rate,
            self.sample_format,
        )
    }

    pub fn rtp_payload_size(&self) -> usize {
        utils::rtp_payload_size(
            self.sample_rate,
            self.packet_time,
            self.channels,
            self.sample_format,
        )
    }

    pub fn rtp_packet_size(&self) -> usize {
        utils::rtp_packet_size(
            self.sample_rate,
            self.packet_time,
            self.channels,
            self.sample_format,
        )
    }

    pub fn samples_per_link_offset_buffer(&self) -> usize {
        utils::samples_per_link_offset_buffer(self.channels, self.link_offset, self.sample_rate)
    }

    pub fn rtp_buffer_size(&self) -> usize {
        utils::rtp_buffer_size(
            self.link_offset,
            self.packet_time,
            self.sample_rate,
            self.channels,
            self.sample_format,
        )
    }

    pub fn to_link_offset(&self, samples: usize) -> usize {
        utils::to_link_offset(samples, self.sample_rate)
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
        status: StatusApi,
        local_ip: IpAddr,
        audio_buffer: AudioBuffer,
    ) -> Result<Self, RxError> {
        let (channel, commands) = mpsc::channel(1);

        subsys.start(SubsystemBuilder::new("rtp/rx", move |s| async move {
            let cancel = s.create_cancellation_token();
            let mut actor: RtpRxActor =
                RtpRxActor::new(s, commands, status, local_ip, audio_buffer);
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

type RxSubsysErr = Box<dyn std::error::Error + Send + Sync + 'static>;

struct RtpRxActor {
    commands: mpsc::Receiver<RxFunction>,
    used_channels: usize,
    status: StatusApi,
    subsys: SubsystemHandle,
    receiver_ids: HashMap<String, ReceiverId>,
    active_receivers: Box<[Option<(RxDescriptor, NestedSubsystem<RxSubsysErr>)>]>,
    matrix: OutputMatrix,
    local_ip: IpAddr,
    active_ports: Box<[Option<usize>]>,
    audio_buffer: AudioBuffer,
}

impl Drop for RtpRxActor {
    fn drop(&mut self) {
        self.subsys.request_shutdown();
    }
}

impl Actor for RtpRxActor {
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
                else => break,
            }
        }

        Ok(())
    }
}

impl RtpRxActor {
    fn new(
        subsys: SubsystemHandle,
        commands: mpsc::Receiver<RxFunction>,
        status: StatusApi,
        local_ip: IpAddr,
        audio_buffer: AudioBuffer,
    ) -> Self {
        let max_channels = audio_buffer.format.audio_format.frame_format.channels;
        let receiver_ids = HashMap::new();
        let active_receivers = init_buffer(max_channels, |_| None);
        let matrix = OutputMatrix::default(max_channels);
        let active_ports = init_buffer(max_channels, |_| None);

        RtpRxActor {
            subsys,
            commands,
            used_channels: 0,
            status,
            receiver_ids,
            active_receivers,
            matrix,
            local_ip,
            active_ports,
            audio_buffer,
        }
    }

    fn max_channels(&self) -> usize {
        self.audio_buffer.format.audio_format.frame_format.channels
    }

    fn buffer_len(&self) -> usize {
        self.audio_buffer.format.buffer_len
    }

    async fn create_receiver(&mut self, sdp: SessionDescription) -> RxResult<()> {
        // TODO fail for receivers with incompatible sample rate

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
                return Err(RxError::MaxChannelsExceeded(self.max_channels()));
            }
        };
        let desc = RxDescriptor::new(receiver_id, &sdp, self.buffer_len())?;
        if self.used_channels + desc.channels > self.max_channels() {
            return Err(RxError::MaxChannelsExceeded(self.max_channels()));
        }
        log::info!("Creating receiver '{receiver_id}' for session '{session_id}' …",);
        self.used_channels += desc.channels;
        self.receiver_ids.insert(session_id, desc.id);

        let socket = create_rx_socket(&sdp, self.local_ip).await?;

        let changed_ports = self
            .matrix
            .auto_route(receiver_id, desc.channels, desc.clone())
            .expect("port availability must be checked first");

        self.activate_ports(changed_ports).await;

        let rx_desc = desc.clone();
        let rx_stat = self.status.clone();
        let matrix = self.matrix.clone();
        let buffer = self.audio_buffer.clone();
        let subsys = move |s| async move {
            let receive_loop = ReceiveLoop::new(rx_desc, socket, rx_stat, matrix, buffer);
            receive_loop.run(s).await;
            Ok::<(), RxError>(())
        };
        let rx_subsys: NestedSubsystem<RxSubsysErr> = self
            .subsys
            .start(SubsystemBuilder::new(receiver_id.to_string(), subsys));

        self.active_receivers[receiver_id] = Some((desc.clone(), rx_subsys));

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

    async fn delete_receiver(&mut self, receiver_id: ReceiverId) -> RxResult<()> {
        if receiver_id >= self.active_receivers.len() {
            return Err(RxError::InvalidReceiverId(receiver_id));
        }

        if let Some((desc, subsys)) = self.active_receivers[receiver_id].take() {
            log::info!(
                "Deleting receiver '{receiver_id}' for session '{}' …",
                desc.session_id
            );

            subsys.initiate_shutdown();

            let changed_ports = self.matrix.auto_unroute(receiver_id);

            self.deactivate_ports(changed_ports).await;

            self.receiver_ids.remove(&desc.session_id());

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
            self.max_channels()
        );
        self.status
            .publish(Status::UsedOutputChannels(
                self.used_channels,
                self.max_channels(),
            ))
            .await?;
        Ok(())
    }

    async fn activate_ports(&mut self, changed_ports: Vec<usize>) {
        for port in changed_ports {
            self.active_ports[port] = self.matrix.mapping.get(&port).map(|ch| ch.0.channel_nr);
        }
    }

    async fn deactivate_ports(&mut self, changed_ports: Vec<usize>) {
        for port in changed_ports {
            self.active_ports[port] = None;
        }
    }
}

async fn create_rx_socket(
    sdp: &SessionDescription,
    local_ip: IpAddr,
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
        (IpAddr::V4(ipv4_addr), IpAddr::V4(local_ip)) => {
            create_ipv4_rx_socket(ipv4_addr, local_ip, port)?
        }
        (IpAddr::V6(ipv6_addr), IpAddr::V6(local_ip)) => {
            create_ipv6_rx_socket(ipv6_addr, local_ip, port)?
        }
        (IpAddr::V4(_), IpAddr::V6(_)) => Err(RxError::Other(
            "Cannot receive IPv4 stream when bound to local IPv6 address".to_owned(),
        ))?,
        (IpAddr::V6(_), IpAddr::V4(_)) => Err(RxError::Other(
            "Cannot receive IPv6 stream when bound to local IPv4 address".to_owned(),
        ))?,
    };

    let tokio_socket = UdpSocket::from_std(socket.into())?;

    Ok(tokio_socket)
}

struct ReceiveLoop {
    desc: RxDescriptor,
    socket: UdpSocket,
    rtp_buffer: Box<[u8]>,
    min_buffer_usage: (usize, usize),
    status: StatusApi,
    out_of_order_packets: usize,
    dropped_packets: usize,
    matrix: OutputMatrix,
    audio_buffer: AudioBuffer,
}

impl ReceiveLoop {
    fn new(
        desc: RxDescriptor,
        socket: UdpSocket,
        status: StatusApi,
        matrix: OutputMatrix,
        audio_buffer: AudioBuffer,
    ) -> Self {
        let rtp_buffer = init_buffer(desc.rtp_packet_size(), |_| 0u8);

        Self {
            desc,
            socket,
            rtp_buffer,
            min_buffer_usage: (0, 0),
            status,
            out_of_order_packets: 0,
            dropped_packets: 0,
            matrix,
            audio_buffer,
        }
    }

    async fn run(mut self, subsys: SubsystemHandle) {
        let mut interval = interval(Duration::from_millis(200));
        loop {
            select! {
                _ = subsys.on_shutdown_requested() => break,
                Ok((len, addr)) = self.socket.recv_from(&mut self.rtp_buffer) => self.process_packet(len, addr).await,
                _ = interval.tick() => self.report_statistics().await,
                else => break,
            }
        }

        log::info!("Receiver {} stopped.", self.desc.id);
    }

    async fn process_packet(&mut self, len: usize, addr: SocketAddr) {
        if addr.ip() == self.desc.origin_ip {
            self.update_buffer_usage();
            self.audio_buffer
                .insert(&self.rtp_buffer[0..len], &self.desc, &self.matrix);
        } else {
            log::warn!("Received packet from wrong sender: {addr}");
        }
    }

    fn update_buffer_usage(&mut self) {
        // TODO
    }

    async fn report_statistics(&mut self) {
        let used = self.min_buffer_usage.0;
        let available = self.min_buffer_usage.1;
        let percent = if available == 0 {
            0
        } else {
            (used * 100) / available
        };
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
        // TODO
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
