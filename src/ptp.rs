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

pub mod statime_linux;

use pnet::datalink::NetworkInterface;
use statime_linux::{statime_linux, PtpClock};
use std::net::IpAddr;
use worterbuch_client::Worterbuch;

pub async fn ptp(
    iface: NetworkInterface,
    ip: IpAddr,
    wb: Worterbuch,
    root_key: String,
) -> PtpClock {
    statime_linux(iface, ip, wb, root_key).await
}
