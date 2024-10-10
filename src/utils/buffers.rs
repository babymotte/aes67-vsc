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
use crate::{
    error::RtpResult,
    rtp::{Channel, OutputMatrix, RxDescriptor},
    AudioFormat, AudioSystemConfig, BufferConfig, BufferFormat, FrameFormat, SampleFormat,
    SampleMetadata, SampleMetadataMut, SampleReader,
};
use rtp_rs::{RtpReader, Seq};
use serde_json::json;
use shared_memory::ShmemConf;
use std::{
    slice::from_raw_parts_mut,
    sync::{Arc, Mutex},
};

pub fn init_buffer<T>(size: usize, init: impl Fn(usize) -> T) -> Box<[T]> {
    (0..size).map(init).collect::<Vec<T>>().into()
}

pub fn playout_buffer(desc: RxDescriptor) -> (PlayoutBufferWriter, PlayoutBufferReader) {
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

pub struct PlayoutBufferWriter {
    data: Arc<Box<[Mutex<(bool, Box<[u8]>)>]>>,
    pub desc: RxDescriptor,
}

impl PlayoutBufferWriter {
    pub fn insert(&mut self, rtp_packet: &[u8], link_offset: f32) {
        let rtp = match RtpReader::new(rtp_packet) {
            Ok(it) => it,
            Err(e) => {
                log::warn!("received malformed rtp packet: {e:?}");
                return;
            }
        };

        let reception_timestamp =
            MediaClockTimestamp::new(rtp.timestamp(), self.desc.sample_rate) - self.desc.rtp_offset;

        let playout_timestamp = reception_timestamp.playout_time(link_offset);

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
pub struct PlayoutBufferReader {
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

pub fn create_audio_buffer(format: BufferFormat) -> RtpResult<(AudioBuffer, String)> {
    // fs::remove_file(flink).ok();
    let len = format.bytes_per_buffer();
    log::info!("Creating shared memory buffer with length {} …", len);
    let mut shared_memory = ShmemConf::new()
        .size(format.bytes_per_buffer())
        // .flink(flink)
        .create()?;

    let slice = unsafe { shared_memory.as_slice_mut() };

    for sample in slice.chunks_mut(SampleFormat::Internal.bytes_per_sample()) {
        sample.copy_from_slice(&SampleFormat::init_internal());
    }

    let shared_memory_prt = shared_memory.as_ptr() as usize;
    let id = shared_memory.get_os_id().to_owned();

    log::info!("Created shared memory {id}");

    // TODO store somewhere so it can be dropped on shutdown
    Box::leak(Box::new(shared_memory));

    Ok((
        AudioBuffer {
            shared_memory_prt,
            format,
        },
        id,
    ))
}

pub fn open_audio_buffer(buffer_config: &BufferConfig) -> RtpResult<AudioBuffer> {
    log::info!("Opening shared memory {} …", buffer_config.address);
    let shared_memory = ShmemConf::new().os_id(&buffer_config.address).open()?;
    let buffer_len = unsafe { shared_memory.as_slice().len() };
    let shared_memory_prt = shared_memory.as_ptr() as usize;
    let audio_format = buffer_config.format.audio_format;
    let format = BufferFormat {
        buffer_len,
        audio_format,
    };

    log::info!(
        "Opened shared memory buffer with length {}",
        format.buffer_len
    );

    // TODO store somewhere so it can be dropped on shutdown
    Box::leak(Box::new(shared_memory));

    Ok(AudioBuffer {
        shared_memory_prt,
        format,
    })
}

#[derive(Debug, Clone, Copy)]
pub struct AudioBuffer {
    shared_memory_prt: usize,
    pub format: BufferFormat,
}

impl AudioBuffer {
    pub fn insert(&mut self, rtp_packet: &[u8], desc: &RxDescriptor, matrix: &OutputMatrix) {
        let rtp = match RtpReader::new(rtp_packet) {
            Ok(it) => it,
            Err(e) => {
                log::warn!("received malformed rtp packet: {e:?}");
                return;
            }
        };

        let reception_timestamp =
            MediaClockTimestamp::new(rtp.timestamp(), self.format.audio_format.sample_rate);

        let playout_timestamp = reception_timestamp + self.format.buffer_len / 2;

        let bytes_per_buffer_sample = self
            .format
            .audio_format
            .frame_format
            .sample_format
            .bytes_per_sample();

        // the x scale of the buffer when seen as a two-dimensional array
        let bytes_per_buffer_frame = self.format.audio_format.frame_format.bytes_per_frame();
        // the y scale of the buffer when seen as a two-dimensional array
        let frames_per_buffer = self.format.frames_per_buffer();

        let buffer = self.buffer();

        // write data into buffer
        // TODO make sure this does not break at wrap around!
        let frame_start = playout_timestamp.timestamp as usize % frames_per_buffer;

        for (packet_frame_index, packet_frame) in
            rtp.payload().chunks(desc.bytes_per_frame()).enumerate()
        {
            let buffer_frame_index = (frame_start + packet_frame_index) % frames_per_buffer;
            for (ch_nr, sample) in packet_frame.chunks(desc.bytes_per_sample()).enumerate() {
                let receiver_channel = Channel::new(desc.id, ch_nr);
                if let Some(outputs) = matrix.get_outputs(&receiver_channel) {
                    for output in outputs {
                        let index_in_frame = output * bytes_per_buffer_sample;
                        let sample_start =
                            buffer_frame_index * bytes_per_buffer_frame + index_in_frame;
                        let sample_end = sample_start + bytes_per_buffer_sample;
                        SampleFormat::to_internal_format(
                            sample,
                            &mut buffer[sample_start..sample_end],
                            rtp.sequence_number(),
                        );
                    }
                }
            }
        }
    }

    pub fn disable_channels(&mut self, desc: &RxDescriptor, matrix: &OutputMatrix) {
        let bytes_per_buffer_sample = self
            .format
            .audio_format
            .frame_format
            .sample_format
            .bytes_per_sample();

        // the x scale of the buffer when seen as a two-dimensional array
        let bytes_per_buffer_frame = self.format.audio_format.frame_format.bytes_per_frame();
        // the y scale of the buffer when seen as a two-dimensional array
        let frames_per_buffer = self.format.frames_per_buffer();

        let buffer = self.buffer();

        for buffer_frame_index in 0..frames_per_buffer {
            for ch_nr in 0..desc.channels {
                let receiver_channel = Channel::new(desc.id, ch_nr);
                if let Some(outputs) = matrix.get_outputs(&receiver_channel) {
                    for output in outputs {
                        let index_in_frame = output * bytes_per_buffer_sample;
                        let sample_start =
                            buffer_frame_index * bytes_per_buffer_frame + index_in_frame;
                        let mut sample =
                            &mut buffer[sample_start..sample_start + bytes_per_buffer_sample];
                        sample.set_disabled(true);
                        sample.set_muted(true);
                    }
                }
            }
        }
    }

    pub fn read<S: Default>(
        &mut self,
        playout_timestamp: MediaClockTimestamp,
        seq: Seq,
        channel: usize,
        output_buffer: &mut [S],
    ) -> (Seq, Option<MediaClockTimestamp>)
    where
        SampleFormat: SampleReader<S>,
    {
        let bytes_per_buffer_sample = self
            .format
            .audio_format
            .frame_format
            .sample_format
            .bytes_per_sample();
        let bytes_per_buffer_frame = self.format.audio_format.frame_format.bytes_per_frame();
        let sample_format = self.format.audio_format.frame_format.sample_format;
        let frames_per_buffer = self.format.frames_per_buffer();

        let buffer = self.buffer();

        // TODO make sure this does not break at wrap around!
        let frame_start = playout_timestamp.timestamp as usize % frames_per_buffer;

        let mut underrun_timestamp = None;

        let mut sequence_number = seq;

        for (frame, sample) in output_buffer.iter_mut().enumerate() {
            let buffer_frame_index = (frame_start + frame) % frames_per_buffer;

            let sample_index_in_frame = channel * bytes_per_buffer_sample;
            let sample_start = buffer_frame_index * bytes_per_buffer_frame + sample_index_in_frame;
            let sample_end = sample_start + bytes_per_buffer_sample;
            let buf = &buffer[sample_start..sample_end];

            let disabled = buf.is_disabled();
            let seq = buf.sequence_number();
            // TODO this will fail to detect missed packets if link offset == packet time, figure out at which sample exactly we expect the sequence number to go up
            let underrun = !(sequence_number == seq || sequence_number.precedes(seq));
            let muted = disabled || underrun || buf.is_muted();

            if underrun {
                if !disabled && underrun_timestamp.is_none() {
                    underrun_timestamp = Some(playout_timestamp + frame);
                }
                sequence_number = sequence_number.next();
            } else {
                sequence_number = seq;
            }

            *sample = if muted {
                S::default()
            } else {
                sample_format.read_sample(buf)
            };
        }

        (sequence_number, underrun_timestamp)
    }

    fn buffer<'a>(&'a mut self) -> &'a mut [u8] {
        unsafe {
            from_raw_parts_mut(
                self.shared_memory_prt as *mut u8,
                self.format.bytes_per_buffer(),
            )
        }
    }
}

pub fn open_shared_memory_buffers(
    mem_conf: &AudioSystemConfig,
) -> RtpResult<(AudioBuffer, AudioBuffer)> {
    // TODO read shared memory file paths from stdin
    // let input_flink = env::var("AES67_VSC_SHARED_MEMORY_INPUT_BUFFER")
    //     .unwrap_or("/tmp/aes67-vsc-input-buffer".to_owned());
    // let output_flink = env::var("AES67_VSC_SHARED_MEMORY_OUTPUT_BUFFER")
    //     .unwrap_or("/tmp/aes67-vsc-output-buffer".to_owned());

    let input_buffer = open_audio_buffer(&mem_conf.input_buffer)?;
    let output_buffer = open_audio_buffer(&mem_conf.output_buffer)?;

    Ok((input_buffer, output_buffer))
}

pub fn cretae_shared_memory_buffers(
    inputs: usize,
    outputs: usize,
    sample_rate: usize,
    link_offset: f32,
) -> RtpResult<(AudioBuffer, AudioBuffer)> {
    // TODO read shared memory file paths from stdin
    // let input_flink = env::var("AES67_VSC_SHARED_MEMORY_INPUT_BUFFER")
    //     .unwrap_or("/tmp/aes67-vsc-input-buffer".to_owned());
    // let output_flink = env::var("AES67_VSC_SHARED_MEMORY_OUTPUT_BUFFER")
    //     .unwrap_or("/tmp/aes67-vsc-output-buffer".to_owned());

    let input_frame_format = FrameFormat {
        channels: inputs,
        sample_format: SampleFormat::Internal,
    };
    let output_frame_format = FrameFormat {
        channels: outputs,
        sample_format: SampleFormat::Internal,
    };

    let input_audio_format = AudioFormat {
        sample_rate,
        frame_format: input_frame_format,
    };
    let output_audio_format = AudioFormat {
        sample_rate,
        frame_format: output_frame_format,
    };

    let input_buffer_format = BufferFormat::for_rtp_playout_buffer(link_offset, input_audio_format);
    let output_buffer_format =
        BufferFormat::for_rtp_playout_buffer(link_offset, output_audio_format);

    let (input_buffer, iid) = create_audio_buffer(input_buffer_format)?;
    let (output_buffer, oid) = create_audio_buffer(output_buffer_format)?;

    let ibc = BufferConfig {
        address: iid,
        format: input_buffer_format,
    };

    let obc = BufferConfig {
        address: oid,
        format: output_buffer_format,
    };

    let asc = AudioSystemConfig {
        input_buffer: ibc,
        output_buffer: obc,
    };

    println!("{}", json!(asc));

    Ok((input_buffer, output_buffer))
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
            sample_format: crate::SampleFormat::L16,
            channels: 2,
            id: 0,
            link_offset: 1.0,
            origin_ip: "0.0.0.0".parse().unwrap(),
            packet_time: 1.0,
            rtp_offset: 0,
            sample_rate: 48000,
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

        pobw.insert(&packet, 1.0);

        let ts = MediaClockTimestamp::new(96, 48000);

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
