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

use super::frames_per_link_offset_buffer;
use crate::error::RtpResult;
use libc::{clock_gettime, timespec, CLOCK_TAI};
use std::{
    cmp::Ordering,
    fmt::Display,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    ops::{Add, Sub},
    sync::{atomic::AtomicI64, Arc},
    time::Duration,
};
use tokio::{io::AsyncReadExt, net::TcpStream, spawn, sync::mpsc, time::sleep};

#[derive(Debug, Clone, Copy)]
pub struct MediaClockTimestamp {
    pub timestamp: u32,
    sample_rate: usize,
}

impl Display for MediaClockTimestamp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.timestamp)
    }
}

impl MediaClockTimestamp {
    pub fn new(timestamp: u32, sample_rate: usize) -> Self {
        Self {
            timestamp,
            sample_rate,
        }
    }

    pub fn jump_to(&self, timestamp: u32) -> Self {
        Self {
            timestamp,
            sample_rate: self.sample_rate,
        }
    }

    pub fn next(&self) -> Self {
        let timestamp = self.timestamp.wrapping_add(1);

        Self {
            timestamp,
            sample_rate: self.sample_rate,
        }
    }

    pub fn previous(&self) -> Self {
        let timestamp = self.timestamp.wrapping_sub(1);

        Self {
            timestamp,
            sample_rate: self.sample_rate,
        }
    }

    pub fn playout_time(&self, link_offset: f32) -> MediaClockTimestamp {
        let timestamp = wrap_u64(
            self.timestamp as u64
                + frames_per_link_offset_buffer(link_offset, self.sample_rate as usize) as u64,
        );
        self.jump_to(timestamp)
    }
}

impl From<MediaClockTimestamp> for i64 {
    fn from(value: MediaClockTimestamp) -> Self {
        value.timestamp as i64
    }
}

impl From<MediaClockTimestamp> for u64 {
    fn from(value: MediaClockTimestamp) -> Self {
        value.timestamp as u64
    }
}

impl PartialEq for MediaClockTimestamp {
    fn eq(&self, other: &Self) -> bool {
        self.timestamp == other.timestamp && self.sample_rate == other.sample_rate
    }
}

impl Eq for MediaClockTimestamp {}

impl Add<u32> for MediaClockTimestamp {
    type Output = Self;

    fn add(self, rhs: u32) -> Self::Output {
        Self {
            sample_rate: self.sample_rate,
            timestamp: self.timestamp.wrapping_add(rhs),
        }
    }
}

impl Add<u64> for MediaClockTimestamp {
    type Output = Self;

    fn add(self, rhs: u64) -> Self::Output {
        Self {
            sample_rate: self.sample_rate,
            timestamp: self.timestamp.wrapping_add((rhs % u32::MAX as u64) as u32),
        }
    }
}

impl Add<usize> for MediaClockTimestamp {
    type Output = Self;

    fn add(self, rhs: usize) -> Self::Output {
        self + rhs as u64
    }
}

impl Sub for MediaClockTimestamp {
    type Output = i64;

    fn sub(self, rhs: Self) -> Self::Output {
        let delta = i64::from(self) - i64::from(rhs);
        if delta < i32::MIN as i64 {
            u32::MAX as i64 + 1 + delta
        } else if delta > i32::MAX as i64 {
            delta - u32::MAX as i64 - 1
        } else {
            delta
        }
    }
}

impl Sub<u32> for MediaClockTimestamp {
    type Output = MediaClockTimestamp;

    fn sub(self, rhs: u32) -> Self::Output {
        Self {
            sample_rate: self.sample_rate,
            timestamp: self.timestamp.wrapping_sub(rhs),
        }
    }
}

impl PartialOrd for MediaClockTimestamp {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for MediaClockTimestamp {
    fn cmp(&self, other: &Self) -> Ordering {
        let diff = *self - *other;
        diff.cmp(&0)
    }
}

pub fn wrap_u128(value: u128) -> u32 {
    (value % (u32::MAX as u128 + 1)) as u32
}

pub fn wrap_u64(value: u64) -> u32 {
    (value % (u32::MAX as u64 + 1)) as u32
}

#[derive(Clone)]
pub struct RemoteMediaClock {
    offset: Arc<AtomicI64>,
    sample_rate: usize,
    close: mpsc::Sender<()>,
}

impl RemoteMediaClock {
    pub async fn connect(port: u16, sample_rate: usize) -> RtpResult<Self> {
        let offset = Arc::new(AtomicI64::new(0));
        let task_offset = offset.clone();
        let (close, mut close_rx) = mpsc::channel(1);

        spawn(async move {
            loop {
                match TcpStream::connect(SocketAddr::new(
                    IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
                    port,
                ))
                .await
                {
                    Ok(socket) => {
                        if !read_offset(socket, &task_offset, &mut close_rx).await {
                            break;
                        }
                    }
                    Err(e) => {
                        log::error!(
                            "Could not connect to PTP socket: {e}. Trying again in 5 seconds …"
                        );
                        sleep(Duration::from_secs(5)).await;
                    }
                }
            }
            log::info!("PTP client stopped.");
        });

        Ok(Self {
            offset,
            sample_rate,
            close,
        })
    }

    pub fn media_time(&self) -> MediaClockTimestamp {
        media_time(
            self.offset.load(std::sync::atomic::Ordering::Acquire),
            self.sample_rate,
        )
    }

    pub fn close(&self) {
        self.close.try_send(()).ok();
    }
}

async fn read_offset(
    mut socket: TcpStream,
    task_offset: &Arc<AtomicI64>,
    close: &mut mpsc::Receiver<()>,
) -> bool {
    log::info!("Connected to PTP socket.");
    loop {
        if close.try_recv().is_ok() {
            return false;
        }
        match socket.read_i64().await {
            Ok(offset) => {
                task_offset.store(offset, std::sync::atomic::Ordering::Release);
            }
            Err(e) => {
                log::error!("Lost connection to PTP socket: {e}. Reconnecting in 5 seconds …");
                sleep(Duration::from_secs(5)).await;
                return true;
            }
        }
    }
}

pub fn media_time(offset: i64, sample_rate: usize) -> MediaClockTimestamp {
    let timestamp = wrapped_media_time(sample_rate, offset);
    MediaClockTimestamp {
        timestamp,
        sample_rate,
    }
}

fn wrapped_media_time(sample_rate: usize, offset: i64) -> u32 {
    wrap_u128(raw_media_time(sample_rate, offset))
}

fn raw_media_time(sample_rate: usize, offset: i64) -> u128 {
    let now = system_time();
    let nanos =
        (now.tv_sec * Duration::from_secs(1).as_nanos() as i64 + now.tv_nsec + offset) as u128;
    (nanos * sample_rate as u128) / std::time::Duration::from_secs(1).as_nanos()
}

pub fn system_time() -> timespec {
    let mut tp = timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    if unsafe { clock_gettime(CLOCK_TAI, &mut tp) } == -1 {
        // TODO handle error
    }
    tp
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::utils::frames_per_packet;

    #[test]
    fn media_clock_timestamp_addition_works() {
        let ts_1 = MediaClockTimestamp {
            sample_rate: 48000,
            timestamp: 0,
        };
        assert_eq!(
            ts_1 + 1u32,
            MediaClockTimestamp {
                sample_rate: 48000,
                timestamp: 1,
            }
        );

        let ts_1 = MediaClockTimestamp {
            sample_rate: 48000,
            timestamp: u32::MAX,
        };
        assert_eq!(
            ts_1 + 1u32,
            MediaClockTimestamp {
                sample_rate: 48000,
                timestamp: 0,
            }
        );
    }

    #[test]
    fn media_clock_timestamp_subtraction_works() {
        let ts_1 = MediaClockTimestamp {
            sample_rate: 48000,
            timestamp: 0,
        };
        let ts_2 = MediaClockTimestamp {
            sample_rate: 48000,
            timestamp: 1,
        };
        assert_eq!(ts_2 - ts_1, 1);

        let ts_1 = MediaClockTimestamp {
            sample_rate: 48000,
            timestamp: 0,
        };
        let ts_2 = next_packet(&ts_1);
        assert_eq!(ts_2 - ts_1, frames_per_packet(48000, 1.0) as i64);

        let ts_1 = MediaClockTimestamp {
            sample_rate: 48000,
            timestamp: u32::MAX,
        };
        let ts_2 = MediaClockTimestamp {
            sample_rate: 48000,
            timestamp: 0,
        };
        assert_eq!(ts_2 - ts_1, 1);
        assert_eq!(ts_1 - ts_2, -1);
    }

    #[test]
    fn media_clock_timestamp_next_works() {
        let ts_1 = MediaClockTimestamp {
            sample_rate: 48000,
            timestamp: 1,
        };
        assert_eq!(next_packet(&ts_1).timestamp, 49);

        let ts_1 = MediaClockTimestamp {
            sample_rate: 48000,
            timestamp: u32::MAX - 20,
        };
        assert_eq!(next_packet(&ts_1).timestamp, 27);
    }

    #[test]
    fn media_clock_timestamp_compare_works_without_wraparound() {
        let ts_1 = MediaClockTimestamp {
            sample_rate: 48000,
            timestamp: 1,
        };
        let ts_2 = next_packet(&ts_1);
        assert!(ts_1 < ts_2);
    }

    #[test]
    fn media_clock_timestamp_compare_works_with_wraparound_of_one() {
        let ts_1 = MediaClockTimestamp {
            sample_rate: 48000,
            timestamp: u32::MAX,
        };
        let ts_2 = MediaClockTimestamp {
            sample_rate: 48000,
            timestamp: 0,
        };
        assert!(ts_1 < ts_2);
    }

    #[test]
    fn media_clock_timestamp_compare_works_with_wraparound_of_multiple() {
        let ts_1 = MediaClockTimestamp {
            sample_rate: 48000,
            timestamp: u32::MAX - 4,
        };
        let ts_2 = next_packet(&ts_1);
        assert!(ts_1 < ts_2);
        let ts_1 = MediaClockTimestamp {
            sample_rate: 48000,
            timestamp: u32::MAX - 4,
        };
        let ts_2 = next_packet(&ts_1);
        assert!(ts_1 < ts_2);
    }

    #[test]
    fn media_clock_timestamp_sorting_without_wraparound_works() {
        let ts_1 = MediaClockTimestamp {
            sample_rate: 48000,
            timestamp: 0,
        };
        let ts_2 = next_packet(&ts_1);
        let ts_3 = next_packet(&ts_2);
        let ts_4 = next_packet(&ts_2);

        let mut vec = vec![ts_3, ts_4, ts_1, ts_2];
        vec.sort();
        assert_eq!(vec, vec![ts_1, ts_2, ts_3, ts_4]);
    }

    #[test]
    fn media_clock_timestamp_sorting_with_wraparound_works() {
        let ts_1 = MediaClockTimestamp {
            sample_rate: 48000,
            timestamp: u32::MAX - ((1.5 * frames_per_packet(48000, 1.0) as f32) as u32),
        };
        let ts_2 = next_packet(&ts_1);
        let ts_3 = next_packet(&ts_2);
        let ts_4 = next_packet(&ts_2);

        let mut vec = vec![ts_3, ts_4, ts_2, ts_1];
        vec.sort();
        assert_eq!(vec, vec![ts_1, ts_2, ts_3, ts_4]);
    }

    #[ignore = "too slow to run in debug mode"]
    #[test]
    fn media_clock_timestamp_playout_time_is_consistent() {
        let mut ts = MediaClockTimestamp {
            sample_rate: 48000,
            timestamp: 0,
        };

        let mut last_playout_time = None;
        for i in 0..u32::MAX {
            let playout_time = ts.playout_time(4.0).timestamp;
            if let Some(lpt) = last_playout_time {
                assert_eq!(wrap_u64(lpt as u64 + 1), playout_time);
            }
            ts = ts.next();
            last_playout_time = Some(playout_time);
            if i % (u32::MAX / 100) == 0 {
                eprintln!("{}%", i / (u32::MAX / 100));
            }
        }

        assert_eq!(last_playout_time, Some(190));
    }

    fn next_packet(ts: &MediaClockTimestamp) -> MediaClockTimestamp {
        let increment = frames_per_packet(ts.sample_rate as usize, 1.0) as u32;
        *ts + increment
    }
}
