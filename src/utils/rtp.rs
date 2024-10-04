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
use rtp_rs::{RtpReader, Seq};
use std::collections::BTreeSet;
use tokio::sync::mpsc;

use super::MediaClockTimestamp;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Packet {
    pub timestamp: MediaClockTimestamp,
    pub sequence_number: Seq,
    pub payload: Vec<u8>,
}

impl PartialOrd for Packet {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Packet {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.timestamp.cmp(&other.timestamp)
    }
}

#[derive(Debug, Clone)]
pub(crate) enum SequenceBufferEvent {
    PacketDrop,
    OutOfOrderPacket,
}

pub(crate) struct RtpSequenceBuffer {
    // unbounded, stores out-of-sequence packets
    sequence_buffer: BTreeSet<Packet>,
    playout_buffer: Vec<Packet>,
    desc: RxDescriptor,
    next_seq: Option<Seq>,
    event_tx: mpsc::UnboundedSender<SequenceBufferEvent>,
}

impl RtpSequenceBuffer {
    pub fn new(desc: RxDescriptor, event_tx: mpsc::UnboundedSender<SequenceBufferEvent>) -> Self {
        let sequence_buffer = BTreeSet::new();
        // oversize buffers, we really don't want to re-allocate during playout
        let output_buffer = Vec::with_capacity(10 * desc.samples_per_link_offset_buffer());
        Self {
            sequence_buffer,
            playout_buffer: output_buffer,
            desc,
            next_seq: None,
            event_tx,
        }
    }

    pub fn update<'a>(&'a mut self, packet: &[u8]) -> &'a mut Vec<Packet> {
        self.insert(packet);
        self.flush()
    }

    fn insert(&mut self, packet: &[u8]) {
        if let Ok(rtp) = RtpReader::new(packet) {
            let sequence_number = rtp.sequence_number();
            let timestamp = MediaClockTimestamp::new(rtp.timestamp(), &self.desc);
            let payload = rtp.payload().to_vec();

            let packet = Packet {
                payload,
                sequence_number,
                timestamp,
            };

            self.do_insert(packet);
        }
    }

    fn do_insert(&mut self, packet: Packet) {
        if let Some(seq) = self.next_seq {
            if seq == packet.sequence_number {
                let mut next_seq = packet.sequence_number.next();
                self.playout_buffer.push(packet);
                while let Some(p) = self.sequence_buffer.first() {
                    if p.sequence_number == next_seq {
                        let packet = self.sequence_buffer.pop_first().expect("can't be missing");
                        next_seq = packet.sequence_number.next();
                        self.playout_buffer.push(packet);
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
            self.playout_buffer.push(packet);
        }
    }

    fn flush<'a>(&'a mut self) -> &'a mut Vec<Packet> {
        &mut self.playout_buffer
    }
}

#[cfg(test)]
mod test {

    use super::*;
    use rtp_rs::RtpPacketBuilder;
    use std::time::Duration;
    use tokio::{spawn, time::sleep};

    const BYTES_PER_SAMPLE: usize = 1;

    #[tokio::test]
    async fn sequence_buffer_does_not_change_non_wrapping_consistent_sequences() {
        // todo!()
    }

    #[tokio::test]
    async fn sequence_buffer_corrects_non_wrapping_inconsistent_sequences() {
        // todo!()
    }

    #[tokio::test]
    async fn sequence_buffer_does_not_change_wrapping_consistent_sequences() {
        // todo!()
    }

    #[tokio::test]
    async fn sequence_buffer_corrects_wrapping_inconsistent_sequences() {
        // todo!()
    }

    #[tokio::test]
    async fn sequence_buffer_recovers_from_non_wrapping_consistent_sequences_with_missing_packets()
    {
        // todo!()
    }

    #[tokio::test]
    async fn sequence_buffer_recovers_from_wrapping_consistent_sequences_with_missing_packets() {
        // todo!()
    }

    #[tokio::test]
    async fn sequence_buffer_recovers_from_non_wrapping_inconsistent_sequences_with_missing_packets(
    ) {
        // todo!()
    }

    #[tokio::test]
    async fn sequence_buffer_recovers_from_wrapping_inconsistent_sequences_with_missing_packets() {
        // todo!()
    }

    fn next_packet(seq: Seq) -> Vec<u8> {
        // RtpPacketBuilder::new().sequence(seq).timestamp(timestamp)
        todo!()
    }

    fn test_sample_reader(data: &[u8]) -> f32 {
        assert_eq!(data.len(), 1);
        *(data.iter().next().unwrap()) as f32
    }
}
