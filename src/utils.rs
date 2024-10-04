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

mod buffers;
mod time;

pub(crate) use buffers::*;
pub(crate) use time::*;

use std::net::IpAddr;
use thread_priority::{
    set_thread_priority_and_policy, thread_native_id, RealtimeThreadSchedulePolicy, ThreadPriority,
    ThreadSchedulePolicy,
};
use tokio::{process::Command, spawn};

pub fn rtp_header_len() -> usize {
    12
}

pub fn max_samplerate() -> usize {
    96000
}

pub fn max_bit_depth() -> usize {
    32
}

pub fn max_packet_time() -> f32 {
    4.0
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

pub async fn open_browser(ip: IpAddr, port: u16) {
    spawn(async move {
        Command::new("xdg-open")
            .arg(format!("http://{ip}:{port}"))
            .output()
            .await
            .ok();
    });
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn frames_per_link_offset_buffer_works() {
        assert_eq!(192, frames_per_link_offset_buffer(4.0, 1.0, 48_000));
    }
}
