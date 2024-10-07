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

use std::sync::{Arc, Mutex};

use rtp_rs::RtpReader;
use tokio::sync::mpsc;

use crate::rtp::RxDescriptor;

use super::{bytes_per_sample, MediaClockTimestamp};

pub fn init_buffer<T>(size: usize, init: impl Fn(usize) -> T) -> Box<[T]> {
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

pub(crate) struct PacketQueueBuffer {
    buffer: Box<[u8]>,
    allocations: Box<[bool]>,
    capacity: usize,
    packet_len: usize,
    len: usize,
}

impl PacketQueueBuffer {
    pub fn new(capacity: usize, packet_len: usize) -> Self {
        let buffer = init_buffer(capacity * packet_len, |_| 0u8);
        let allocations = init_buffer(capacity, |_| false);
        Self {
            buffer,
            allocations,
            capacity,
            packet_len,
            len: 0,
        }
    }

    pub fn insert(&mut self, packet: &[u8]) {
        if packet.len() != self.packet_len {
            panic!(
                "packet has invalid length {} (buffer requires {})",
                packet.len(),
                self.packet_len
            )
        }

        if self.len >= self.capacity {
            panic!("buffer is full");
        }

        for i in 0..self.capacity {
            if !self.allocations[i] {
                let start = i * self.packet_len;
                let end = start + self.packet_len;
                let slot = &mut self.buffer[start..end];
                slot.copy_from_slice(packet);
                self.allocations[i] = true;
                self.len += 1;
                return;
            }
        }
    }

    pub fn drain<'a>(&'a mut self) -> DrainPacketQueueBuffer<'a> {
        DrainPacketQueueBuffer::new(self)
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn is_full(&self) -> bool {
        self.len == self.capacity
    }

    fn take<'a>(&'a mut self, index: usize) -> &'a [u8] {
        if index > self.capacity {
            panic!(
                "index {index} is out of bounds (capacity: {})",
                self.capacity
            )
        }

        if self.is_empty() {
            panic!("buffer is empty");
        }

        if !self.allocations[index] {
            panic!("no entry at index {index}");
        }

        self.allocations[index] = false;
        self.len -= 1;

        let start = index * self.packet_len;
        let end = start + self.packet_len;

        &self.buffer[start..end]
    }
}

#[must_use = "iterators are lazy and do nothing unless consumed"]
pub(crate) struct DrainPacketQueueBuffer<'a> {
    buffer: &'a mut PacketQueueBuffer,
    cursor: usize,
}

impl<'a> DrainPacketQueueBuffer<'a> {
    fn new(buffer: &'a mut PacketQueueBuffer) -> Self {
        Self { buffer, cursor: 0 }
    }

    pub fn next<'b>(&'b mut self) -> Option<&'b [u8]> {
        if self.buffer.is_empty() {
            return None;
        }

        while !self.buffer.allocations[self.cursor] {
            self.cursor += 1;
            if self.cursor >= self.buffer.capacity {
                panic!("reached the end of the buffer, it should be empty");
            }
        }

        let slice = self.buffer.take(self.cursor);

        Some(slice)
    }
}

pub(crate) fn playout_buffer(desc: RxDescriptor) -> (PlayoutBufferWriter, PlayoutBufferReader) {
    // - data needs to be held in a non-blocking datastructure
    // - we can assume that while there will be concurrent reads and writes on the buffer,
    //   each packet in the buffer would be stored in its own cell and we can assume that
    //   there will never be concurrent reads and writes in the same cell
    // - this means we can either accept the possibility of data corruption within one cell
    //   or we can make access to individual cells blocking because the lock would always
    //   be obtained immediately
    // - this will beed some kind of cursor which must be non-blocking but thread safe, since
    //   it must be read by both the producer and the consumer at the same time and updated
    //   by the producer

    let data_write = Arc::new(init_buffer(2 * desc.packets_in_link_offset(), |_| {
        Mutex::new((false, init_buffer(desc.rtp_payload_size(), |_| 0u8)))
    }));

    let data_read = data_write.clone();

    (
        PlayoutBufferWriter {
            data: data_write,
            desc: desc.clone(),
        },
        PlayoutBufferReader {
            data: data_read,
            desc,
        },
    )
}

pub(crate) struct PlayoutBufferWriter {
    data: Arc<Box<[Mutex<(bool, Box<[u8]>)>]>>,
    pub desc: RxDescriptor,
}

impl PlayoutBufferWriter {
    pub fn insert(&mut self, rtp_packet: &[u8]) {
        let rtp = match RtpReader::new(rtp_packet) {
            Ok(it) => it,
            Err(e) => {
                log::warn!("received malformed rtp packet: {e:?}");
                return;
            }
        };

        let reception_timestamp = MediaClockTimestamp::new(
            rtp.timestamp(),
            self.desc.sampling_rate,
            self.desc.link_offset,
        );

        let playout_timestamp = reception_timestamp.playout_time();

        // TODO make sure this does not break at wrap around!
        let packet_index = (playout_timestamp.timestamp as usize / self.desc.frames_per_packet())
            % self.data.len();

        // dbg!(raw_index, packet_index);

        let slice = &self.data[packet_index];
        let mut slice = slice.lock().expect("mutex is poisoned");
        slice.1.copy_from_slice(rtp.payload());
        slice.0 = true;
    }
}

#[derive(Clone)]
pub(crate) struct PlayoutBufferReader {
    data: Arc<Box<[Mutex<(bool, Box<[u8]>)>]>>,
    pub desc: RxDescriptor,
}

impl PlayoutBufferReader {
    pub fn read<S>(
        &mut self,
        playout_timestamp: MediaClockTimestamp,
        channel: usize,
        sample_reader: fn(&[u8]) -> S,
    ) -> Option<S> {
        // TODO make sure this does not break at wrap around!
        let packet_index = (playout_timestamp.timestamp as usize / self.desc.frames_per_packet())
            % self.data.len();

        let packet = &self.data[packet_index];
        let packet = packet.lock().expect("mutex is poisoned");
        if packet.0 {
            let sample_start = (playout_timestamp.timestamp as usize
                % self.desc.frames_per_packet())
                * self.desc.bytes_per_frame()
                + channel * self.desc.bytes_per_sample();
            let sample_end = sample_start + self.desc.bytes_per_sample();
            let raw_sample = &packet.1[sample_start..sample_end];
            let sample = sample_reader(raw_sample);
            // TODO how to find out all samples of packet have been consumed?
            return Some(sample);
        }

        None
    }
}

#[cfg(test)]
mod test {
    use super::{playout_buffer, PacketQueueBuffer};
    use crate::{
        rtp::RxDescriptor,
        utils::{init_buffer, MediaClockTimestamp},
    };
    use rtp_rs::RtpPacketBuilder;

    #[test]
    fn packet_queue_buffer_works() {
        let mut buf = PacketQueueBuffer::new(4, 3);

        assert_eq!(buf.len(), 0);
        assert!(buf.is_empty());

        let pack_1 = [1, 2, 3];
        let pack_2 = [4, 5, 6];
        let pack_3 = [7, 8, 9];
        let pack_4 = [10, 11, 12];

        buf.insert(&pack_1);
        assert_eq!(buf.len(), 1);

        buf.insert(&pack_2);
        assert_eq!(buf.len(), 2);

        buf.insert(&pack_3);
        assert_eq!(buf.len(), 3);

        buf.insert(&pack_4);
        assert_eq!(buf.len(), 4);

        assert!(!buf.is_empty());
        assert!(buf.is_full());

        let mut iter = buf.drain();

        assert_eq!(iter.next(), Some(&[1, 2, 3][..]));
        assert_eq!(iter.next(), Some(&[4, 5, 6][..]));
        assert_eq!(iter.next(), Some(&[7, 8, 9][..]));
        assert_eq!(iter.next(), Some(&[10, 11, 12][..]));
        assert_eq!(iter.next(), None);

        assert!(buf.is_empty());
    }

    #[test]
    fn playout_buffer_works() {
        let desc = RxDescriptor {
            bit_depth: 16,
            channels: 2,
            id: 0,
            link_offset: 1.0,
            origin_ip: "0.0.0.0".parse().unwrap(),
            packet_time: 1.0,
            rtp_offset: 0,
            sampling_rate: 48000,
            session_id: 0,
            session_version: 0,
            session_name: "nein".to_owned(),
        };

        let (mut pobw, mut pobr) = playout_buffer(desc);

        let payload = init_buffer(192, |j| (j / 2) as u8);

        let packet = RtpPacketBuilder::new()
            .timestamp(48)
            .payload(&payload)
            .payload_type(97)
            .build()
            .unwrap();

        pobw.insert(&packet);

        let ts = MediaClockTimestamp::new(96, 48000, 1.0);

        for i in 0..48 {
            let sample_l = pobr.read(ts + i as u32, 0, test_sample_reader).unwrap();
            assert_eq!((2 * i, sample_l), (2 * i, vec![2 * i, 2 * i]));
            let sample_r = pobr.read(ts + i as u32, 1, test_sample_reader).unwrap();
            assert_eq!(
                (2 * i + 1, sample_r),
                (2 * i + 1, vec![2 * i + 1, 2 * i + 1])
            );
        }
    }

    fn test_sample_reader(raw: &[u8]) -> Vec<u8> {
        raw.to_vec()
    }
}
