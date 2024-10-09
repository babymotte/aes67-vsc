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

use error::RxError;
use rtp::RxDescriptor;
use serde::{Deserialize, Serialize};
use std::{str::FromStr, time::Duration};

pub mod actor;
pub mod discovery_cleanup;
pub mod error;
pub mod ptp;
pub mod rtp;
pub mod sap;
pub mod status;
pub mod utils;

pub type ReceiverId = usize;
pub type TransmitterId = usize;

#[derive(Debug, Clone, Copy, PartialEq, Deserialize, Serialize)]
pub struct BufferFormat {
    pub buffer_len: usize,
    pub audio_format: AudioFormat,
}

impl BufferFormat {
    pub fn for_rtp_payload(value: &RxDescriptor) -> Self {
        let audio_format: AudioFormat = value.into();
        let buffer_len = audio_format.bytes_per_buffer(value.link_offset);
        Self {
            buffer_len,
            audio_format,
        }
    }

    pub fn for_rtp_playout_buffer(link_offset: f32, audio_format: AudioFormat) -> Self {
        let buffer_len = audio_format.bytes_per_buffer(3.0 * link_offset);
        Self {
            buffer_len,
            audio_format,
        }
    }

    pub fn frames_per_buffer(&self) -> usize {
        self.buffer_len / self.audio_format.frame_format.bytes_per_frame()
    }

    pub fn bytes_per_buffer(&self) -> usize {
        self.audio_format.frame_format.bytes_per_frame() * self.frames_per_buffer()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
pub struct AudioFormat {
    pub sample_rate: usize,
    pub frame_format: FrameFormat,
}

impl AudioFormat {
    pub fn bytes_per_buffer(&self, link_offset: f32) -> usize {
        self.samples_per_link_offset_buffer(link_offset)
            * self.frame_format.sample_format.bytes_per_sample()
    }

    pub fn samples_per_link_offset_buffer(&self, link_offset: f32) -> usize {
        self.frame_format.channels * self.frames_per_link_offset_buffer(link_offset)
    }

    pub fn frames_per_link_offset_buffer(&self, link_offset: f32) -> usize {
        f32::ceil(
            (self.sample_rate as f32 * link_offset) / Duration::from_secs(1).as_millis() as f32,
        ) as usize
    }
}

impl From<&RxDescriptor> for AudioFormat {
    fn from(value: &RxDescriptor) -> Self {
        Self {
            sample_rate: value.sample_rate,
            frame_format: value.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
pub struct FrameFormat {
    pub channels: usize,
    pub sample_format: SampleFormat,
}

impl From<&RxDescriptor> for FrameFormat {
    fn from(value: &RxDescriptor) -> Self {
        Self {
            channels: value.channels,
            sample_format: value.sample_format.clone(),
        }
    }
}

impl FrameFormat {
    pub fn bytes_per_frame(&self) -> usize {
        self.samples_per_frame() * self.sample_format.bytes_per_sample()
    }

    pub fn samples_per_frame(&self) -> usize {
        self.channels
    }

    pub fn sample_index_in_buffer_frame(&self, channel: usize) -> usize {
        channel * self.sample_format.bytes_per_sample()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
pub enum SampleFormat {
    // TODO implement other sample formats
    L16,
    L24,
}

impl FromStr for SampleFormat {
    type Err = RxError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "L24" => Ok(SampleFormat::L24),
            other => Err(RxError::UnknownSampleFormat(other.to_owned())),
        }
    }
}

pub trait SampleReader<S> {
    fn read_sample(&self, buffer: &[u8]) -> S;
}

impl SampleReader<f32> for SampleFormat {
    fn read_sample(&self, buffer: &[u8]) -> f32 {
        self.read_f32(buffer)
    }
}

impl SampleReader<i32> for SampleFormat {
    fn read_sample(&self, buffer: &[u8]) -> i32 {
        self.read_i32(buffer)
    }
}

impl SampleFormat {
    fn read_f32(&self, buffer: &[u8]) -> f32 {
        match self {
            SampleFormat::L16 => bytes_to_f32_2_bytes(&buffer),
            SampleFormat::L24 => bytes_to_f32_3_bytes(&buffer),
        }
    }

    fn read_i32(&self, buffer: &[u8]) -> i32 {
        match self {
            SampleFormat::L16 => bytes_to_i32_2_bytes(&buffer),
            SampleFormat::L24 => bytes_to_i32_3_bytes(&buffer),
        }
    }

    pub fn bytes_per_sample(&self) -> usize {
        match self {
            SampleFormat::L16 => 2,
            SampleFormat::L24 => 3,
        }
    }
}

fn pad_sample(payload: &[u8]) -> [u8; 4] {
    let mut sample = [0, 0, 0, 0];
    for (i, b) in payload.iter().enumerate() {
        sample[i] = *b;
    }
    sample
}

fn bytes_to_f32_2_bytes(bytes: &[u8]) -> f32 {
    let value = bytes_to_i32_2_bytes(bytes);
    if value >= 0 {
        value as f32 / i16::MAX as f32
    } else {
        (value + 1) as f32 / i16::MAX as f32
    }
}

fn bytes_to_i32_2_bytes(bytes: &[u8]) -> i32 {
    i16::from_be_bytes([bytes[0], bytes[1]]) as i32
}

fn bytes_to_f32_3_bytes(bytes: &[u8]) -> f32 {
    let value = bytes_to_i32_3_bytes(bytes);

    if value >= 0 {
        value as f32 / 0x7FFFFF as f32 // Max 24-bit signed value
    } else {
        (value + 1) as f32 / 0x7FFFFF as f32 // Max 24-bit signed value
    }
}

fn bytes_to_i32_3_bytes(bytes: &[u8]) -> i32 {
    let mut value =
        (((bytes[0] as i32) << 16) | ((bytes[1] as i32) << 8) | (bytes[2] as i32)) as i32;

    // Sign extend from 24-bit to 32-bit
    if value & 0x800000 != 0 {
        value |= !0xFFFFFF;
    }
    value
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioSystemConfig {
    pub input_buffer: BufferConfig,
    pub output_buffer: BufferConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BufferConfig {
    pub format: BufferFormat,
    pub address: String,
}
