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

use crate::rtp::{audio_system::RtpSample, RxDescriptor};
use rtp_rs::{RtpReader, RtpReaderError, Seq};
use std::{cmp::Ordering, collections::BTreeSet, iter::Map, net::IpAddr, slice::Chunks};
use thread_priority::{
    set_thread_priority_and_policy, thread_native_id, RealtimeThreadSchedulePolicy, ThreadPriority,
    ThreadSchedulePolicy,
};
use tokio::{process::Command, spawn, sync::mpsc};

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
    rtp_header_len() * (channels * max_samplerate() * max_packet_time() * max_bit_depth())
        / 8
        / 1000
}

pub fn bytes_per_sample(bit_depth: usize) -> usize {
    bit_depth / 8
}

pub fn bytes_per_frame(channels: usize, bit_depth: usize) -> usize {
    channels * bytes_per_sample(bit_depth)
}

pub fn frames_per_packet(sampling_rate: usize, packet_time: f32) -> usize {
    f32::ceil((sampling_rate as f32 * packet_time) / 1000.0) as usize
}

pub fn samples_per_packet(channels: usize, sampling_rate: usize, packet_time: f32) -> usize {
    channels * frames_per_packet(sampling_rate, packet_time)
}

pub fn packets_in_link_offset(link_offset: f32, packet_time: f32) -> usize {
    f32::ceil(link_offset / packet_time) as usize
}

pub fn frames_per_link_offset_buffer(
    link_offset: f32,
    packet_time: f32,
    sampling_rate: usize,
) -> usize {
    packets_in_link_offset(link_offset, packet_time) * frames_per_packet(sampling_rate, packet_time)
}

pub fn rtp_payload_size(
    sampling_rate: usize,
    packet_time: f32,
    channels: usize,
    bit_depth: usize,
) -> usize {
    frames_per_packet(sampling_rate, packet_time) * bytes_per_frame(channels, bit_depth)
}

pub fn rtp_packet_size(
    sampling_rate: usize,
    packet_time: f32,
    channels: usize,
    bit_depth: usize,
) -> usize {
    rtp_header_len() + rtp_payload_size(sampling_rate, packet_time, channels, bit_depth)
}

pub fn samples_per_link_offset_buffer(
    channels: usize,
    link_offset: f32,
    packet_time: f32,
    sampling_rate: usize,
) -> usize {
    channels * frames_per_link_offset_buffer(link_offset, packet_time, sampling_rate)
}

pub fn rtp_buffer_size(
    link_offset: f32,
    packet_time: f32,
    sampling_rate: usize,
    channels: usize,
    bit_depth: usize,
) -> usize {
    packets_in_link_offset(link_offset, packet_time)
        * rtp_packet_size(sampling_rate, packet_time, channels, bit_depth)
}

pub fn to_link_offset(samples: usize, sampling_rate: usize) -> usize {
    f32::ceil((samples as f32 * 1000.0) / sampling_rate as f32) as usize
}

pub(crate) struct RtpIter<'a> {
    consistent: Option<ConsistentRtpIterator<'a>>,
    inconsistent: Option<InconsistentRtpIterator<'a>>,
}

impl<'a> RtpIter<'a> {
    pub fn new(buffer: &'a [u8], packet_len: usize) -> Result<Self, RtpReaderError> {
        if buffer.len() % packet_len != 0 {
            log::warn!(
                "Buffer len {} is not a multiple of packet len {}, this is likely a bug.",
                buffer.len(),
                packet_len
            );
        }

        if is_consistent(&buffer, packet_len)? {
            Ok(Self {
                consistent: Some(ConsistentRtpIter(buffer, packet_len).into_iter()),
                inconsistent: None,
            })
        } else {
            log::debug!("inconsistent packet sequence");
            Ok(Self {
                consistent: None,
                inconsistent: Some(InconsistentRtpIterator::new(buffer, packet_len)?),
            })
        }
    }
}

impl<'a> Iterator for RtpIter<'a> {
    type Item = RtpReader<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(iter) = self.consistent.as_mut() {
            iter.next()
        } else if let Some(iter) = self.inconsistent.as_mut() {
            iter.next()
        } else {
            None
        }
    }
}

type ConsistentRtpIterator<'a> = Map<Chunks<'a, u8>, fn(&[u8]) -> RtpReader>;
struct ConsistentRtpIter<'a>(&'a [u8], usize);

impl<'a> IntoIterator for ConsistentRtpIter<'a> {
    type Item = RtpReader<'a>;

    type IntoIter = ConsistentRtpIterator<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.chunks(self.1).map(to_rtp)
    }
}

fn to_rtp(buf: &[u8]) -> RtpReader {
    RtpReader::new(buf).expect("already checked")
}

struct InconsistentRtpIterator<'a> {
    packet_len: usize,
    seq_min: Seq,
    seq_max: Seq,
    next_seq: Option<Seq>,
    current: usize,
    buffer: &'a [u8],
}

impl<'a> InconsistentRtpIterator<'a> {
    fn new(buffer: &'a [u8], packet_len: usize) -> Result<Self, RtpReaderError> {
        let mut seq_min = Seq::from(u16::MAX);
        let mut seq_max = Seq::from(0);

        let first = RtpReader::new(&buffer[0..packet_len])?;
        let might_wrap =
            u16::from(first.sequence_number()) as usize > u16::MAX as usize - 2 * buffer.len();

        for chunk in buffer.chunks(packet_len) {
            let rtp = RtpReader::new(chunk)?;
            let seq = rtp.sequence_number();

            if seq < seq_min && (!might_wrap || (u16::from(seq) > u16::MAX / 2)) {
                seq_min = seq
            }
            if seq > seq_max && (!might_wrap || (u16::from(seq) < u16::MAX / 2)) {
                seq_max = seq
            }
        }

        let next_seq = Some(seq_min);
        let current = 0;

        // log::debug!("Building inconsistent RTP iterator");
        // dbg!(buffer.len());
        // dbg!(&first.sequence_number());
        // dbg!(&seq_min);
        // dbg!(&seq_max);
        // dbg!(&next_seq);
        // dbg!(&current);
        // dbg!(&might_wrap);

        Ok(InconsistentRtpIterator {
            packet_len,
            seq_min,
            seq_max,
            next_seq,
            current,
            buffer,
        })
    }
}

impl<'a> Iterator for InconsistentRtpIterator<'a> {
    type Item = RtpReader<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let start = self.current;
        self.current = start + self.packet_len;

        if self.current > self.buffer.len() {
            return None;
        }

        let item = RtpReader::new(&self.buffer[start..self.current]).expect("already checked");

        if let Some(next_seq) = self.next_seq {
            if item.sequence_number() == next_seq {
                if self.current == self.buffer.len() {
                    self.next_seq = None;
                } else {
                    self.next_seq = Some(next_seq.next());
                }
                Some(item)
            } else {
                let next = self.next();
                self.current = (self.current as i32 - 2 * self.packet_len as i32).max(0) as usize;
                next
            }
        } else {
            None
        }
    }
}

fn is_consistent(buf: &[u8], packet_len: usize) -> Result<bool, RtpReaderError> {
    let mut last_seq: Option<Seq> = None;

    for chunk in buf.chunks(packet_len) {
        let rtp = RtpReader::new(chunk)?;
        let seq = rtp.sequence_number();
        if let Some(last_seq) = last_seq {
            if !last_seq.precedes(seq) {
                return Ok(false);
            }
        }
        last_seq = Some(seq);
    }

    Ok(true)
}

pub(crate) fn init_buffer<T>(size: usize, init: impl Fn(usize) -> T) -> Box<[T]> {
    (0..size).map(init).collect::<Vec<T>>().into()
}

pub struct DoubleBuffer<T> {
    pub tx: mpsc::Sender<Box<[T]>>,
    pub rx: mpsc::Receiver<Box<[T]>>,
}

impl<T: Default> DoubleBuffer<T> {
    pub async fn new(size: usize) -> (DoubleBuffer<T>, DoubleBuffer<T>) {
        let buffer_a = init_buffer(size, |_| T::default());
        let buffer_b = init_buffer(size, |_| T::default());

        let (producer_tx, consumer_rx) = mpsc::channel(1);
        let (consumer_tx, producer_rx) = mpsc::channel(2);

        producer_tx
            .send(buffer_a)
            .await
            .expect("receiver cannot be dropped at this point");
        consumer_tx
            .send(buffer_b)
            .await
            .expect("receiver cannot be dropped at this point");

        (
            DoubleBuffer {
                tx: producer_tx,
                rx: producer_rx,
            },
            DoubleBuffer {
                tx: consumer_tx,
                rx: consumer_rx,
            },
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Packet {
    timestamp: u32,
    sequence_number: Seq,
    payload: Vec<u8>,
}

impl PartialOrd for Packet {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        match self.timestamp.partial_cmp(&other.timestamp) {
            Some(Ordering::Equal) => {}
            ord => return ord,
        }
        self.sequence_number.partial_cmp(&other.sequence_number)
    }
}

impl Ord for Packet {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match self.timestamp.cmp(&other.timestamp) {
            Ordering::Equal => {}
            ord => return ord,
        }
        self.sequence_number.cmp(&other.sequence_number)
    }
}

#[derive(Debug, Clone)]
pub(crate) enum SequenceBufferEvent {
    PacketDrop,
    OutOfOrderPacket,
}

pub(crate) struct RtpSequenceBuffer<S: Copy> {
    // unbounded, stores out-of-sequence packets
    sequence_buffer: BTreeSet<Packet>,
    // initial output buffer, used to queue up to link-offset samples before playing out
    output_buffer: Vec<Packet>,
    playout_buffer: Vec<RtpSample<S>>,
    init_phase: bool,
    sample_reader: fn(&[u8]) -> S,
    desc: RxDescriptor,
    next_seq: Option<Seq>,
    event_tx: mpsc::UnboundedSender<SequenceBufferEvent>,
}

impl<S: Copy> RtpSequenceBuffer<S> {
    pub fn new(
        desc: RxDescriptor,
        sample_reader: fn(&[u8]) -> S,
        event_tx: mpsc::UnboundedSender<SequenceBufferEvent>,
    ) -> Self {
        let sequence_buffer = BTreeSet::new();
        // oversize buffers, we really don't want to re-allocate during playout
        let output_buffer = Vec::with_capacity(10 * desc.packets_in_link_offset());
        let playout_buffer = Vec::with_capacity(10 * desc.samples_per_link_offset_buffer());
        Self {
            sequence_buffer,
            output_buffer,
            playout_buffer,
            init_phase: true,
            sample_reader,
            desc,
            next_seq: None,
            event_tx,
        }
    }

    pub fn update<'a>(&'a mut self, packet: &[u8]) -> &'a mut Vec<RtpSample<S>> {
        self.insert(packet);
        self.flush()
    }

    fn insert(&mut self, packet: &[u8]) {
        if let Ok(rtp) = RtpReader::new(packet) {
            let sequence_number = rtp.sequence_number();
            let timestamp = rtp.timestamp();
            let payload = rtp.payload().to_vec();

            let packet = Packet {
                payload,
                sequence_number,
                timestamp,
            };

            self.do_insert(packet);
        }

        if self.init_phase && self.output_buffer.len() >= self.desc.packets_in_link_offset() {
            self.init_phase = false;
        }
    }

    fn do_insert(&mut self, packet: Packet) {
        if let Some(seq) = self.next_seq {
            if seq == packet.sequence_number {
                let mut next_seq = packet.sequence_number.next();
                self.output_buffer.push(packet);
                while let Some(p) = self.sequence_buffer.first() {
                    if p.sequence_number == next_seq {
                        let packet = self.sequence_buffer.pop_first().expect("can't be missing");
                        next_seq = packet.sequence_number.next();
                        self.output_buffer.push(packet);
                    } else {
                        break;
                    }
                }
                self.next_seq = Some(next_seq);
            } else {
                self.event_tx
                    .send(SequenceBufferEvent::OutOfOrderPacket)
                    .ok();
                self.sequence_buffer.insert(packet);
                while self.sequence_buffer.len() > self.desc.packets_in_link_offset() {
                    self.event_tx.send(SequenceBufferEvent::PacketDrop).ok();
                    self.sequence_buffer.pop_first();
                }
            }
        } else {
            self.next_seq = Some(packet.sequence_number.next());
            self.output_buffer.push(packet);
        }
    }

    fn flush<'a>(&'a mut self) -> &'a mut Vec<RtpSample<S>> {
        if !self.init_phase {
            for packet in self.output_buffer.drain(..) {
                for raw_sample in packet.payload.chunks(self.desc.bytes_per_sample()) {
                    let sample = (self.sample_reader)(raw_sample);
                    // TODO once PTP is in place, calculate correct playout time for sample
                    let playout_time = 0;
                    self.playout_buffer.push(RtpSample(playout_time, sample));
                }
            }
        }

        &mut self.playout_buffer
    }

    pub(crate) fn reset(&mut self) {
        self.init_phase = true;
    }
}

pub fn set_realtime_priority() {
    let pid = thread_native_id();
    if let Err(e) = set_thread_priority_and_policy(
        thread_native_id(),
        ThreadPriority::Max,
        ThreadSchedulePolicy::Realtime(RealtimeThreadSchedulePolicy::Fifo),
    ) {
        log::warn!("Could not set thread priority: {e}");
    } else {
        log::info!("Successfully set real time priority for thread {pid}.");
    }
}

#[cfg(test)]
mod test {

    use super::*;
    use std::time::Duration;
    use tokio::{spawn, time::sleep};

    #[tokio::test]
    async fn double_buffers_do_double_buffer_things() {
        let (mut prod, mut cons) = DoubleBuffer::new(10).await;

        let join1 = spawn(async move {
            for i in 1..5 {
                eprintln!("I/O delay");
                sleep(Duration::from_millis(3)).await;
                let mut buf = prod.rx.recv().await.unwrap();
                eprintln!("write {i}");
                for j in 0..buf.len() {
                    buf[j] = i as f32;
                }
                prod.tx.send(buf).await.unwrap();
            }
        });

        let join2 = spawn(async move {
            for i in 0..5 {
                let buf = cons.rx.recv().await.unwrap();
                eprintln!("read {i}: {:?}", buf);
                assert_eq!(
                    *buf,
                    [
                        i as f32, i as f32, i as f32, i as f32, i as f32, i as f32, i as f32,
                        i as f32, i as f32, i as f32
                    ]
                );
                // unwrap would fail on the last iteration
                cons.tx.send(buf).await.ok();
            }
        });

        join1.await.unwrap();
        join2.await.unwrap();
    }
}

pub async fn open_browser(ip: IpAddr, port: u16) {
    spawn(async move {
        Command::new("xdg-open")
            .arg(format!("http://{ip}:{port}"))
            .output()
            .await
            .ok();
    });
}
