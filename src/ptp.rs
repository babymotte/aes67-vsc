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

pub(crate) mod statime_linux;

use crate::{
    error::{PtpError, PtpResult},
    utils::{wrap_u128, MediaClockTimestamp},
};
use ::statime_linux::config::{Config, DelayType, NetworkMode, PortConfig};
use libc::{
    clock_gettime, timespec, CLOCK_MONOTONIC_RAW, EACCES, EFAULT, EINVAL, ENODEV, ENOTSUP, EPERM,
};
use pnet::datalink::NetworkInterface;
use statime::{
    config::{AcceptableMasterList, ClockIdentity},
    time::{Duration, Time},
    Clock,
};
use statime_linux::{statime_linux, SystemClock};
use std::{
    net::{IpAddr, SocketAddr},
    path::Path,
};
use timestamped_socket::interface::InterfaceName;

#[derive(Debug, Clone)]
pub struct PtpClock {
    ppm: f64,
    offset: i128,
}

impl Default for PtpClock {
    fn default() -> Self {
        Self {
            ppm: 1_000_000.0,
            offset: 0,
        }
    }
}

impl Clock for PtpClock {
    type Error = PtpError;

    fn now(&self) -> Time {
        log::info!("clock polled");
        Time::from_nanos(self.now_nanos())
    }

    fn step_clock(&mut self, offset: Duration) -> Result<statime::time::Time, Self::Error> {
        let nanos_rounded = offset.nanos_rounded();
        self.offset += nanos_rounded;
        log::info!("Offset: {}", nanos_rounded);
        Ok(self.now())
    }

    fn set_frequency(&mut self, ppm: f64) -> Result<statime::time::Time, Self::Error> {
        self.ppm = ppm;
        log::info!("Frequency: {} ppm", ppm);
        Ok(self.now())
    }

    fn set_properties(
        &mut self,
        time_properties_ds: &statime::config::TimePropertiesDS,
    ) -> Result<(), Self::Error> {
        todo!()
    }
}

impl PtpClock {
    fn update(ts: &mut timespec) -> PtpResult<()> {
        match unsafe { clock_gettime(CLOCK_MONOTONIC_RAW, ts) } {
            0 => Ok(()),
            -1 => Err(get_err()),
            _ => Err(PtpError::Unknown),
        }
    }

    fn now_nanos(&self) -> u64 {
        let mut ts: timespec = timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        Self::update(&mut ts).unwrap();
        let raw_nanos =
            std::time::Duration::from_nanos(ts.tv_sec as u64).as_nanos() as u64 + ts.tv_nsec as u64;
        let multiplier = (1_000_000.0 + self.ppm) / 1_000_000.0;

        f64::round(raw_nanos as f64 * multiplier + self.offset as f64) as u64
    }

    pub fn media_clock(&self, sample_rate: usize, link_offset: f32) -> MediaClockTimestamp {
        let timestamp = self.wrapped_media_clock(sample_rate);
        MediaClockTimestamp::new(timestamp, sample_rate, link_offset)
    }

    pub fn wrapped_media_clock(&self, sampling_rate: usize) -> u32 {
        wrap_u128(self.raw_media_clock(sampling_rate))
    }

    fn raw_media_clock(&self, sampling_rate: usize) -> u128 {
        let nanos = self.now_nanos() as u128;
        (nanos * sampling_rate as u128) / std::time::Duration::from_secs(1).as_nanos()
    }
}

pub async fn ptp(iface: NetworkInterface, ip: IpAddr) -> SystemClock {
    run(iface, ip).await
}

fn get_err() -> PtpError {
    match std::io::Error::last_os_error().raw_os_error() {
        Some(EFAULT) => PtpError::ClockError("tp points outside the accessible address space"),
        Some(EINVAL) => PtpError::ClockError("invalid clockid"),
        Some(ENOTSUP) => PtpError::ClockError("operation not supported"),
        Some(ENODEV) => PtpError::ClockError("clock device not available"),
        Some(EPERM) | Some(EACCES) => PtpError::ClockError("insufficiant privileges"),
        Some(_) => PtpError::ClockError("unknown clock error"),
        None => todo!(),
    }
}

async fn run(iface: NetworkInterface, ip: IpAddr) -> SystemClock {
    let mac = iface.mac.expect("we already checked this");
    let mac = [mac.0, mac.1, mac.2, mac.3, mac.4, mac.5];

    let domain = 0;
    let identity = Some(ClockIdentity::from_mac_address(mac));
    let loglevel = ::statime_linux::tracing::LogLevel::Info;
    let observability = ::statime_linux::config::ObservabilityConfig {
        observation_path: Some(
            Path::new("/home/mbachmann/Downloads/ptpd/log/observe.log").to_path_buf(),
        ),
        observation_permissions: 0o777,
        metrics_exporter_listen: "127.0.0.1:9975".parse().unwrap(),
    };
    let path_trace = true;
    let ports = vec![port_config(ip)];
    let priority1 = 255;
    let priority2 = 255;
    let sdo_id = 0x000;
    let virtual_system_clock = true;

    let config = Config {
        domain,
        identity,
        loglevel,
        observability,
        path_trace,
        ports,
        priority1,
        priority2,
        sdo_id,
        virtual_system_clock,
    };
    statime_linux(config).await
}

#[derive(Debug, Clone)]
struct MasterList();

impl AcceptableMasterList for MasterList {
    fn is_acceptable(&self, _: ClockIdentity) -> bool {
        // TODO ???
        true
    }
}

fn port_config(ip: IpAddr) -> PortConfig {
    let interface = InterfaceName::from_socket_addr(SocketAddr::new(ip, 9091))
        .expect("no interface")
        .expect("no interface");
    let acceptable_master_list = None;
    let hardware_clock = None;
    let network_mode = NetworkMode::Ipv4;
    let announce_interval = 1;
    let sync_interval = 0;
    let announce_receipt_timeout = 3;
    let master_only = false;
    let delay_asymmetry = 0;
    let delay_mechanism = DelayType::E2E;
    let delay_interval = 1;

    PortConfig {
        interface,
        acceptable_master_list,
        hardware_clock,
        network_mode,
        announce_interval,
        sync_interval,
        announce_receipt_timeout,
        master_only,
        delay_asymmetry,
        delay_mechanism,
        delay_interval,
    }
}
