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

pub use buffers::*;
use pnet::datalink;
pub use time::*;

use std::{net::IpAddr, time::Duration};
use thread_priority::{
    set_thread_priority_and_policy, thread_native_id, RealtimeThreadSchedulePolicy, ThreadPriority,
    ThreadSchedulePolicy,
};
use tokio::{process::Command, spawn};

use crate::SampleFormat;

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

pub fn bytes_per_frame(channels: usize, sample_format: SampleFormat) -> usize {
    channels * sample_format.bytes_per_sample()
}

pub fn frames_per_packet(sample_rate: usize, packet_time: f32) -> usize {
    f32::ceil((sample_rate as f32 * packet_time) / 1000.0) as usize
}

pub fn samples_per_packet(channels: usize, sample_rate: usize, packet_time: f32) -> usize {
    channels * frames_per_packet(sample_rate, packet_time)
}

pub fn packets_in_link_offset(link_offset: f32, packet_time: f32) -> usize {
    f32::ceil(link_offset / packet_time) as usize
}

pub fn frames_per_link_offset_buffer(link_offset: f32, sample_rate: usize) -> usize {
    f32::ceil((sample_rate as f32 * link_offset) / Duration::from_secs(1).as_millis() as f32)
        as usize
}

pub fn link_offset_buffer_size(
    channels: usize,
    link_offset: f32,
    sample_rate: usize,
    sample_format: SampleFormat,
) -> usize {
    samples_per_link_offset_buffer(channels, link_offset, sample_rate)
        * sample_format.bytes_per_sample()
}

pub fn rtp_payload_size(
    sample_rate: usize,
    packet_time: f32,
    channels: usize,
    sample_format: SampleFormat,
) -> usize {
    frames_per_packet(sample_rate, packet_time) * bytes_per_frame(channels, sample_format)
}

pub fn rtp_packet_size(
    sample_rate: usize,
    packet_time: f32,
    channels: usize,
    sample_format: SampleFormat,
) -> usize {
    rtp_header_len() + rtp_payload_size(sample_rate, packet_time, channels, sample_format)
}

pub fn samples_per_link_offset_buffer(
    channels: usize,
    link_offset: f32,
    sample_rate: usize,
) -> usize {
    channels * frames_per_link_offset_buffer(link_offset, sample_rate)
}

pub fn rtp_buffer_size(
    link_offset: f32,
    packet_time: f32,
    sample_rate: usize,
    channels: usize,
    sample_format: SampleFormat,
) -> usize {
    packets_in_link_offset(link_offset, packet_time)
        * rtp_packet_size(sample_rate, packet_time, channels, sample_format)
}

pub fn to_link_offset(samples: usize, sample_rate: usize) -> usize {
    f32::ceil((samples as f32 * 1000.0) / sample_rate as f32) as usize
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

pub fn print_ifaces() {
    let mut out = "No network interface specified. Available network interfaces:\n".to_owned();

    for iface in datalink::interfaces() {
        if iface.is_up()
            && iface.is_running()
            && !iface.is_loopback()
            && !iface.ips.is_empty()
            && iface.mac.is_some()
        {
            out += &format!("\t{}\n", iface.name);
        }
    }

    eprintln!("{out}");
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn frames_per_link_offset_buffer_works() {
        assert_eq!(192, frames_per_link_offset_buffer(4.0, 48_000));
    }
}
