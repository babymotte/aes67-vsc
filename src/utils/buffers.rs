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

use super::MediaClockTimestamp;
use crate::rtp::RxDescriptor;
use rtp_rs::RtpReader;
use std::sync::{Arc, Mutex};

pub fn init_buffer<T>(size: usize, init: impl Fn(usize) -> T) -> Box<[T]> {
    (0..size).map(init).collect::<Vec<T>>().into()
}

pub(crate) fn playout_buffer(desc: RxDescriptor) -> (PlayoutBufferWriter, PlayoutBufferReader) {
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
    use super::playout_buffer;
    use crate::{
        rtp::RxDescriptor,
        utils::{init_buffer, MediaClockTimestamp},
    };
    use rtp_rs::RtpPacketBuilder;

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
