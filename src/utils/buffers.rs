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

use tokio::sync::mpsc;

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

#[cfg(test)]
mod test {
    use super::PacketQueueBuffer;

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
}
