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
        let buffer_len = audio_format.bytes_per_buffer(4.0 * link_offset);
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
    Internal,
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
    fn read_sample(&self, buffer: &mut [u8]) -> Option<S>;
}

impl SampleReader<f32> for SampleFormat {
    fn read_sample(&self, buffer: &mut [u8]) -> Option<f32> {
        self.read_f32(buffer)
    }
}

impl SampleReader<i32> for SampleFormat {
    fn read_sample(&self, buffer: &mut [u8]) -> Option<i32> {
        self.read_i32(buffer)
    }
}

impl SampleFormat {
    fn read_f32(&self, buffer: &mut [u8]) -> Option<f32> {
        match self {
            SampleFormat::L16 => Some(bytes_to_f32_2_bytes(buffer)),
            SampleFormat::L24 => Some(bytes_to_f32_3_bytes(buffer)),
            SampleFormat::Internal => internal_bytes_to_f32(buffer),
        }
    }

    fn read_i32(&self, buffer: &mut [u8]) -> Option<i32> {
        match self {
            SampleFormat::L16 => Some(bytes_to_i32_2_bytes(buffer)),
            SampleFormat::L24 => Some(bytes_to_i32_3_bytes(buffer)),
            SampleFormat::Internal => internal_bytes_to_i32(buffer),
        }
    }

    pub fn bytes_per_sample(&self) -> usize {
        match self {
            SampleFormat::L16 => 2,
            SampleFormat::L24 => 3,
            SampleFormat::Internal => 4,
        }
    }

    pub fn to_internal_format(source: &[u8], target: &mut [u8]) {
        if !target.len() == 4 {
            panic!("target buffer must have length 4 but was {}", target.len())
        }

        target[0] = 0u8;
        let target = match source.len() {
            2 => {
                target[0].set_format(SampleFormat::L16);
                &mut target[2..]
            }
            3 => &mut target[1..],
            len => panic!("soure buffer must have length 2 or 3 but was {len}"),
        };
        target.copy_from_slice(source);
    }
}

fn internal_bytes_to_f32(bytes: &mut [u8]) -> Option<f32> {
    if bytes[0].is_consumed() {
        return None;
    }
    // bytes[0].set_consumed(true);
    if bytes[0].is_muted() {
        return Some(0.0);
    }
    // TODO apply other controls
    match bytes[0].get_format() {
        SampleFormat::L16 => Some(bytes_to_f32_2_bytes(&bytes[2..4])),
        SampleFormat::L24 => Some(bytes_to_f32_3_bytes(&bytes[1..4])),
        SampleFormat::Internal => panic!("recursive use of internal sample format"),
    }
}

fn internal_bytes_to_i32(bytes: &mut [u8]) -> Option<i32> {
    if bytes[0].is_consumed() {
        return None;
    }
    // bytes[0].set_consumed(true);
    if bytes[0].is_muted() {
        return Some(0);
    }
    // TODO apply other controls
    match bytes[0].get_format() {
        SampleFormat::L16 => Some(bytes_to_i32_2_bytes(&bytes[2..4])),
        SampleFormat::L24 => Some(bytes_to_i32_3_bytes(&bytes[1..4])),
        SampleFormat::Internal => panic!("recursive use of internal sample format"),
    }
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

pub trait SampleMetadata {
    fn is_consumed(&self) -> bool;
    fn set_consumed(&mut self, consumed: bool);
    fn get_format(&self) -> SampleFormat;
    fn set_format(&mut self, sample_format: SampleFormat);
    fn is_muted(&self) -> bool;
    fn set_muted(&mut self, muted: bool);
    fn is_phase_inverted(&self) -> bool;
    fn set_phase_inverted(&mut self, phase_inverted: bool);
}

impl SampleMetadata for u8 {
    fn is_consumed(&self) -> bool {
        (self >> 7) & 1 == 1
    }

    fn set_consumed(&mut self, value: bool) {
        if value {
            *self |= 0b10000000;
        } else {
            *self &= 0b01111111;
        }
    }

    fn get_format(&self) -> SampleFormat {
        match (self >> 6) & 1 {
            1 => SampleFormat::L16,
            _ => SampleFormat::L24,
        }
    }

    fn set_format(&mut self, value: SampleFormat) {
        if value == SampleFormat::L16 {
            *self |= 0b01000000;
        } else {
            *self &= 0b10111111;
        }
    }

    fn is_muted(&self) -> bool {
        (self >> 5) & 1 == 1
    }

    fn set_muted(&mut self, value: bool) {
        if value {
            *self |= 0b00100000;
        } else {
            *self &= 0b11011111;
        }
    }

    fn is_phase_inverted(&self) -> bool {
        (self >> 4) & 1 == 1
    }

    fn set_phase_inverted(&mut self, value: bool) {
        if value {
            *self |= 0b00010000;
        } else {
            *self &= 0b11101111;
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SampleFormat;

    #[test]
    fn test_u8_is_consumed() {
        let byte = 0b10001010u8;
        assert!(
            byte.is_consumed(),
            "Expected is_consumed to be true when most significant bit is 1."
        );

        let byte = 0b00101001u8;
        assert!(
            !byte.is_consumed(),
            "Expected is_consumed to be false when most significant bit is 0."
        );
    }

    #[test]
    fn test_u8_set_consumed() {
        let mut byte = 0b11100100u8;
        byte.set_consumed(false);
        assert!(
            !byte.is_consumed(),
            "Expected is_consumed to be false after setting it to false."
        );

        byte.set_consumed(true);
        assert!(
            byte.is_consumed(),
            "Expected is_consumed to be true after setting it to true."
        );
    }

    #[test]
    fn test_u8_get_format() {
        let byte = 0b01110010u8;
        assert_eq!(
            byte.get_format(),
            SampleFormat::L16,
            "Expected format to be L16 when second most significant bit is 1."
        );

        let byte = 0b00110010u8;
        assert_eq!(
            byte.get_format(),
            SampleFormat::L24,
            "Expected format to be L24 when second most significant bit is 0."
        );
    }

    #[test]
    fn test_u8_set_format() {
        let mut byte = 0b0000000u8;
        byte.set_format(SampleFormat::L16);
        assert_eq!(
            byte.get_format(),
            SampleFormat::L16,
            "Expected format to be L16 after setting it to L16."
        );

        byte.set_format(SampleFormat::L24);
        assert_eq!(
            byte.get_format(),
            SampleFormat::L24,
            "Expected format to be L24 after setting it to L24."
        );
    }

    #[test]
    fn test_u8_is_muted() {
        let byte = 0b11110010u8;
        assert!(
            byte.is_muted(),
            "Expected is_muted to be true when third most significant bit is 1."
        );

        let byte = 0b11000100u8;
        assert!(
            !byte.is_muted(),
            "Expected is_muted to be false when third most significant bit is 0."
        );
    }

    #[test]
    fn test_u8_set_muted() {
        let mut byte = 0b01010100u8;
        byte.set_muted(true);
        assert!(
            byte.is_muted(),
            "Expected is_muted to be true after setting it to true."
        );

        byte.set_muted(false);
        assert!(
            !byte.is_muted(),
            "Expected is_muted to be false after setting it to false."
        );
    }

    #[test]
    fn test_u8_is_phase_inverted() {
        let byte = 0b11010100u8;
        assert!(
            byte.is_phase_inverted(),
            "Expected is_phase_inverted to be true when fourth most significant bit is 1."
        );

        let byte = 0b10100101u8;
        assert!(
            !byte.is_phase_inverted(),
            "Expected is_phase_inverted to be false when fourth most significant bit is 0."
        );
    }

    #[test]
    fn test_u8_set_phase_inverted() {
        let mut byte = 0b10100101u8;
        byte.set_phase_inverted(true);
        assert!(
            byte.is_phase_inverted(),
            "Expected is_phase_inverted to be true after setting it to true."
        );

        byte.set_phase_inverted(false);
        assert!(
            !byte.is_phase_inverted(),
            "Expected is_phase_inverted to be false after setting it to false."
        );
    }

    #[test]
    fn test_sample_reader_default_values() {
        let mut target = [4, 4, 4, 4];

        let src = [1, 2, 3];
        SampleFormat::to_internal_format(&src, &mut target);
        assert!(!target[0].is_consumed());
        assert_eq!(target[0].get_format(), SampleFormat::L24);
        assert!(!target[0].is_muted());
        assert!(!target[0].is_phase_inverted());

        let src = [1, 2];
        SampleFormat::to_internal_format(&src, &mut target);
        assert!(!target[0].is_consumed());
        assert_eq!(target[0].get_format(), SampleFormat::L16);
        assert!(!target[0].is_muted());
        assert!(!target[0].is_phase_inverted());
    }
}
