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

use crate::rtp::RxDescriptor;
use rtp_rs::{RtpReader, RtpReaderError, Seq};
use std::{iter::Map, net::IpAddr, slice::Chunks, u16};
use thread_priority::{
    set_thread_priority_and_policy, thread_native_id, RealtimeThreadSchedulePolicy, ThreadPriority,
    ThreadSchedulePolicy,
};
use tokio::{process::Command, spawn, sync::mpsc};

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

pub(crate) struct RtpSequenceBuffer {
    buffer: Box<[Option<Box<[u8]>>]>,
}

impl RtpSequenceBuffer {
    pub fn new(desc: &RxDescriptor) -> Self {
        let buffer = init_buffer(desc.packets_in_link_offset(), |_| None);
        Self { buffer }
    }

    pub fn update<'a>(&mut self, packet: &'a [u8]) -> Option<&'a [u8]> {
        // TODO implement actual buffering and return the next Packet to be played out

        if let Ok(rtp) = RtpReader::new(packet) {
            Some(rtp.payload())
        } else {
            None
        }
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
