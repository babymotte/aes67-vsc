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

use crate::{
    actor::{respond, Actor, ActorApi},
    error::{RxError, RxResult},
    utils::{session_id, RtpIter},
};
use core::f32;
use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    BufferSize, HostId, SampleRate, Stream, StreamConfig,
};
use rtp_rs::{RtpReader, Seq};
use sdp::{
    description::common::{Address, ConnectionInformation},
    SessionDescription,
};
use socket2::{Domain, Protocol, SockAddr, Socket, Type};
use std::{
    backtrace,
    cell::Cell,
    collections::HashMap,
    iter,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    thread,
    time::Instant,
};
use tokio::{
    net::UdpSocket,
    runtime, select, spawn,
    sync::{
        mpsc::{self},
        oneshot,
    },
};
use tokio_graceful_shutdown::{SubsystemBuilder, SubsystemHandle};
use tokio_util::sync::CancellationToken;

pub(crate) type ReceiverId = usize;

#[derive(Debug, Clone)]
pub struct RxDescriptor {
    pub id: usize,
    pub bit_depth: usize,
    pub channels: usize,
    pub sampling_rate: usize,
    pub packet_time: f32,
    pub link_offset: f32,
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

        Ok(RxDescriptor {
            id: receiver_id,
            bit_depth,
            channels,
            sampling_rate,
            packet_time,
            link_offset,
        })
    }

    pub fn rtp_header_len(&self) -> usize {
        12
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

    pub fn packets_in_link_offset(&self) -> f32 {
        self.link_offset / self.packet_time
    }

    pub fn frames_per_link_offset_buffer(&self) -> usize {
        f32::ceil(self.packets_in_link_offset() * self.frames_per_packet() as f32) as usize
    }

    pub fn rtp_payload_size(&self) -> usize {
        self.frames_per_packet() * self.bytes_per_frame()
    }

    pub fn rtp_packet_size(&self) -> usize {
        self.rtp_header_len() + self.rtp_payload_size()
    }

    pub fn audio_buffer_size(&self) -> usize {
        self.channels * self.frames_per_link_offset_buffer()
    }

    pub fn rtp_buffer_size(&self) -> usize {
        f32::ceil(self.packets_in_link_offset() * self.rtp_packet_size() as f32) as usize
    }
}

#[derive(Debug)]
enum Dealloc {
    Socket(Socket),
    RxDescriptor(RxDescriptor),
}

#[derive(Debug)]
enum RxThreadEvent {
    BufferOverflow(ReceiverId),
    BufferUnderflow(ReceiverId),
    ThreadStopped,
    Err(RxError),
    Dealloc(Dealloc),
}

#[derive(Debug)]
pub enum RxThreadFunction {
    StartReceiver(ReceiverId, RxDescriptor, UdpSocket, Response<()>),
    StopReceiver(ReceiverId, Response<()>),
}

struct RxThreadDestructor {
    event_tx: mpsc::UnboundedSender<RxThreadEvent>,
}

impl Drop for RxThreadDestructor {
    fn drop(&mut self) {
        self.event_tx.send(RxThreadEvent::ThreadStopped).ok();
    }
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
    ) -> Result<Self, RxError> {
        let (channel, commands) = mpsc::channel(1);

        let (rx_thread_function_tx, rx_thread_function_rx) = mpsc::channel(1);
        let (rx_thread_event_tx, rx_thread_event_rx) = mpsc::unbounded_channel();
        let (stop_tx, stop_rx) = oneshot::channel();

        thread::Builder::new()
            .name("rx-thread".to_string())
            .spawn(move || {
                rx_thread(
                    rx_thread_function_rx,
                    rx_thread_event_tx,
                    stop_rx,
                    max_channels,
                )
            })?;

        subsys.start(SubsystemBuilder::new("rtp/rx", move |s| async move {
            let mut actor = RtpRxActor::new(
                commands,
                rx_thread_function_tx,
                rx_thread_event_rx,
                max_channels,
                link_offset,
            );
            let res = actor.run(s.create_cancellation_token()).await;
            stop_tx.send(()).ok();
            log::info!("RX thread terminated.");
            s.request_shutdown();
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

struct RtpRxActor {
    commands: mpsc::Receiver<RxFunction>,
    receiver_ids: HashMap<String, RxDescriptor>,
    active_receivers: Vec<Option<String>>,
    rx_thread_function_tx: mpsc::Sender<RxThreadFunction>,
    rx_thread_event_rx: mpsc::UnboundedReceiver<RxThreadEvent>,
    max_channels: usize,
    used_channels: usize,
    link_offset: f32,
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

    async fn run(&mut self, cancel_token: CancellationToken) -> RxResult<()> {
        loop {
            select! {
                _ = cancel_token.cancelled() => break,
                Some(msg) = self.commands.recv() => if !self.process_message(msg).await {
                    break;
                },
                Some(e) = self.rx_thread_event_rx.recv() =>  if !self.process_event(e).await {
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
        commands: mpsc::Receiver<RxFunction>,
        rx_thread_function_tx: mpsc::Sender<RxThreadFunction>,
        rx_thread_event_rx: mpsc::UnboundedReceiver<RxThreadEvent>,
        max_channels: usize,
        link_offset: f32,
    ) -> Self {
        let receiver_ids = HashMap::new();
        let active_receivers = vec![None; max_channels];
        RtpRxActor {
            commands,
            receiver_ids,
            active_receivers,
            rx_thread_function_tx,
            rx_thread_event_rx,
            max_channels,
            used_channels: 0,
            link_offset,
        }
    }

    async fn process_event(&mut self, event: RxThreadEvent) -> bool {
        match event {
            RxThreadEvent::BufferOverflow(_) => todo!(),
            RxThreadEvent::BufferUnderflow(_) => todo!(),
            RxThreadEvent::ThreadStopped => false,
            RxThreadEvent::Err(e) => {
                log::error!("Error in receiver thread: {e}");
                false
            }
            RxThreadEvent::Dealloc(it) => {
                drop(it);
                true
            }
        }
    }

    async fn create_receiver(&mut self, sdp: SessionDescription) -> RxResult<()> {
        let session_id = session_id(&sdp);
        let receiver_id = if let Some(id) = self.receiver_ids.get(&session_id).map(|d| d.id) {
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
        let socket = create_rx_socket(&sdp).await?;
        let (tx, rx) = oneshot::channel();
        self.rx_thread_function_tx
            .send(RxThreadFunction::StartReceiver(
                receiver_id,
                desc.clone(),
                socket,
                tx,
            ))
            .await?;
        rx.await??;
        self.receiver_ids.insert(session_id.clone(), desc.clone());
        self.active_receivers[receiver_id] = Some(session_id);
        self.used_channels += desc.channels;
        log::info!("Receiver {receiver_id} created.");
        log::info!(
            "Used up channels: {}/{}",
            self.used_channels,
            self.max_channels
        );
        Ok(())
    }

    async fn delete_receiver(&mut self, receiver_id: ReceiverId) -> RxResult<()> {
        if receiver_id >= self.active_receivers.len() {
            return Err(RxError::InvalidReceiverId(receiver_id));
        }

        let session_id = if let Some(it) = self.active_receivers[receiver_id].take() {
            it
        } else {
            return Err(RxError::InvalidReceiverId(receiver_id));
        };

        log::info!("Deleting receiver '{receiver_id}' for session '{session_id}' …",);

        if let Some(desc) = self.receiver_ids.remove(&session_id) {
            self.used_channels -= desc.channels;
            let (tx, rx) = oneshot::channel();
            self.rx_thread_function_tx
                .send(RxThreadFunction::StopReceiver(receiver_id, tx))
                .await?;
            rx.await??;
            log::info!("Receiver {receiver_id} deleted.",);
            log::info!(
                "Used up channels: {}/{}",
                self.used_channels,
                self.max_channels
            );
        } else {
            log::warn!("Receiver '{receiver_id}' does not exist.");
        }

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

    let Address {
        address,
        range,
        ttl,
    } = address;

    // TODO for unicast addresses check if the IP exists on this machine and reject otherwise
    // TODO for IPv4 check if the TTL allows packets to reach this machine and reject otherwise

    let mut split = address.split('/');
    let ip = split.next();
    let prefix = split.next();
    let ip_addr: IpAddr = if let (Some(ip), Some(prefix)) = (ip, prefix) {
        ip.parse()
            .map_err(|_| RxError::InvalidSdp(format!("invalid ip address: {address}")))?
    } else {
        return Err(RxError::InvalidSdp(format!(
            "invalid ip address: {address}"
        )));
    };

    let socket = match ip_addr {
        IpAddr::V4(ipv4_addr) => create_ipv4_socket(ipv4_addr, port)?,
        IpAddr::V6(ipv6_addr) => create_ipv6_socket(ipv6_addr, port)?,
    };

    let tokio_socket = UdpSocket::from_std(socket.into())?;

    Ok(tokio_socket)
}

fn create_ipv4_socket(ip_addr: Ipv4Addr, port: u16) -> Result<Socket, RxError> {
    log::info!(
        "Creating IPv4 {} RX socket for stream at {}:{}",
        if ip_addr.is_multicast() {
            "multicast"
        } else {
            "unicast"
        },
        ip_addr,
        port
    );

    let local_ip = if ip_addr.is_multicast() {
        Ipv4Addr::UNSPECIFIED
    } else {
        ip_addr
    };
    let local_addr = SocketAddr::new(IpAddr::V4(local_ip), port);

    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    socket.set_reuse_port(true)?;
    socket.bind(&SockAddr::from(local_addr))?;
    if ip_addr.is_multicast() {
        socket.join_multicast_v4(&ip_addr, &local_ip)?
    }
    Ok(socket)
}

fn create_ipv6_socket(ip_addr: Ipv6Addr, port: u16) -> Result<Socket, RxError> {
    log::info!(
        "Creating IPv6 {} RX socket for stream at {}:{}",
        if ip_addr.is_multicast() {
            "multicast"
        } else {
            "unicast"
        },
        ip_addr,
        port
    );

    let local_ip = if ip_addr.is_multicast() {
        Ipv6Addr::UNSPECIFIED
    } else {
        ip_addr
    };
    let local_addr = SocketAddr::new(IpAddr::V6(local_ip), port);

    let socket = Socket::new(Domain::IPV6, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    socket.set_reuse_port(true)?;
    socket.bind(&SockAddr::from(local_addr))?;
    if ip_addr.is_multicast() {
        socket.join_multicast_v6(&ip_addr, 0)?
    }
    Ok(socket)
}

fn rx_thread(
    fun_rx: mpsc::Receiver<RxThreadFunction>,
    event_tx: mpsc::UnboundedSender<RxThreadEvent>,
    stop_rx: oneshot::Receiver<()>,
    max_channels: usize,
) -> RxResult<()> {
    // makes sure a 'thread stopped' event is sent out even in the event of a panic
    let destructor = RxThreadDestructor {
        event_tx: event_tx.clone(),
    };
    let rt = runtime::Builder::new_current_thread().build()?;
    let res = rt.block_on(RxEventLoop::new(event_tx.clone(), max_channels).run(fun_rx, stop_rx));
    drop(destructor);
    res
}

struct RxEventLoop {
    max_bit_depth: usize,
    max_samples_per_frame: usize,
    max_packet_len: usize,
    max_rtp_overhead: usize,
    playout_streams: Box<[Option<CpalStream>]>,
    rx_descriptors: Box<[Option<RxDescriptor>]>,
    event_tx: mpsc::UnboundedSender<RxThreadEvent>,
}

struct CpalStream {
    receiver_id: usize,
    stream: Option<Stream>,
}

impl Drop for CpalStream {
    fn drop(&mut self) {
        log::info!("Dropping cpal stream for receiver '{}'.", self.receiver_id);
        drop(self.stream.take());
    }
}

impl RxEventLoop {
    fn new(event_tx: mpsc::UnboundedSender<RxThreadEvent>, max_channels: usize) -> Self {
        let max_bit_depth = 24;
        let max_samples_per_frame = 384;
        let max_rtp_overhead = 60;
        let max_packet_len = max_bit_depth * max_samples_per_frame + max_rtp_overhead;

        let playout_streams = init_buffer(max_channels, || None);
        let rx_descriptors = init_buffer(max_channels, || None);

        RxEventLoop {
            playout_streams,
            max_bit_depth,
            max_samples_per_frame,
            max_packet_len,
            max_rtp_overhead,
            rx_descriptors,
            event_tx,
        }
    }

    async fn run(
        mut self,
        mut fun_rx: mpsc::Receiver<RxThreadFunction>,
        mut stop_rx: oneshot::Receiver<()>,
    ) -> RxResult<()> {
        loop {
            select! {
                _ = &mut stop_rx => break,
                Some(fun) = fun_rx.recv() =>  {
                    match fun {
                        RxThreadFunction::StartReceiver(index, desc,socket, tx) => {
                            self.start_receiver(index, desc, socket, tx);
                        },
                        RxThreadFunction::StopReceiver(index, tx) => {
                            self.stop_receiver(index, tx);
                        },
                    }
                },
                else => break,
            }
        }
        Ok(())
    }

    fn start_receiver(
        &mut self,
        index: usize,
        desc: RxDescriptor,
        socket: UdpSocket,
        tx: Response<()>,
    ) {
        self.rx_descriptors[index] = Some(desc.clone());
        if let Err(e) = self.playout_stream(index, socket, desc) {
            log::error!("Error playing out receiver stream: {e}");
            tx.send(Err(e)).ok();
        } else {
            tx.send(Ok(())).ok();
        }
    }

    fn stop_receiver(&mut self, index: usize, tx: Response<()>) {
        drop(self.rx_descriptors[index].take());
        drop(self.playout_streams[index].take());
        tx.send(Ok(())).ok();
    }

    fn playout_stream(
        &mut self,
        receiver_id: usize,
        socket: UdpSocket,
        desc: RxDescriptor,
    ) -> RxResult<()> {
        let host = cpal::host_from_id(HostId::Jack).unwrap_or_else(|_| cpal::default_host());
        // let host = cpal::default_host();

        log::info!("Host: {:?}", host.id());

        let device = if let Some(it) = host.default_output_device() {
            log::info!("Output device: {:?}", it.name());
            it
        } else {
            log::error!("no default output device");
            return Err(RxError::NoPlayoutDevice("default".to_owned()));
        };

        log::info!("{:?}", device.default_output_config());

        let config = StreamConfig {
            channels: desc.channels as u16,
            sample_rate: SampleRate(desc.sampling_rate as u32),
            buffer_size: BufferSize::Fixed(desc.frames_per_link_offset_buffer() as u32),
        };

        log::debug!("stream config: {config:?}");

        let (sample_tx, mut sample_rx) = mpsc::channel(2 * desc.rtp_buffer_size());

        let rtp_packet_size = desc.rtp_packet_size();
        let bytes_per_sample = desc.bytes_per_sample();

        spawn(async move {
            let mut socket_buf = init_buffer(desc.rtp_buffer_size(), || 0u8);

            'main: loop {
                for buf in socket_buf.chunks_mut(rtp_packet_size) {
                    let len = match socket.recv(buf).await {
                        Ok(it) => it,
                        Err(e) => {
                            log::error!("Socket I/O error: {e}");
                            break 'main;
                        }
                    };

                    if len != rtp_packet_size {
                        log::warn!(
                            "Unexpected number of bytes read: {} (expected packet length is {})",
                            len,
                            rtp_packet_size
                        );
                    }
                }

                let rtps = match RtpIter::new(&socket_buf, rtp_packet_size) {
                    Ok(it) => it,
                    Err(e) => {
                        log::error!("Could not parse RTP packet: {e:?}");
                        continue 'main;
                    }
                };

                for rtp in rtps {
                    for raw_sample in rtp.payload().chunks(bytes_per_sample) {
                        let sample = read_f32_sample(raw_sample);
                        if let Err(e) = sample_tx.send(sample).await {
                            log::info!("Error forwarding sample: {e}");
                            break 'main;
                        }
                    }
                }
            }
        });

        let stream = device.build_output_stream(
            &config,
            move |output, _| {
                let mut last = 0.0;
                for sample in output.iter_mut() {
                    *sample = sample_rx.try_recv().unwrap_or(last);
                    last = *sample;
                }
            },
            |e| {
                log::error!("Error in cpal stream: {e}");
            },
            None,
        )?;

        if let Err(e) = stream.play() {
            log::error!("Playback error: {e}");
        } else {
            self.playout_streams[receiver_id] = Some(CpalStream {
                receiver_id,
                stream: Some(stream),
            });
        }

        Ok(())
    }
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

fn init_buffer<T>(size: usize, init: impl Fn() -> T) -> Box<[T]> {
    let mut vec = Vec::with_capacity(size);
    vec.resize_with(size, init);
    vec.into()
}
