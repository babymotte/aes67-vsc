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

use crate::error::RxError;
use socket2::{Domain, Protocol, SockAddr, Socket, Type};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

pub(crate) fn create_ipv4_rx_socket(ip_addr: Ipv4Addr, port: u16) -> Result<Socket, RxError> {
    log::info!(
        "Creating IPv4 {} RX socket for stream at {}:{}",
        if ip_addr.is_multicast() {
            "multicast"
        } else {
            "unicast"
        },
        ip_addr,
        port
    );

    let local_ip = if ip_addr.is_multicast() {
        Ipv4Addr::UNSPECIFIED
    } else {
        ip_addr
    };
    let local_addr = SocketAddr::new(IpAddr::V4(local_ip), port);

    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;

    socket.set_reuse_address(true)?;
    // socket.set_reuse_port(true)?;
    socket.set_nonblocking(true)?;

    socket.bind(&SockAddr::from(local_addr))?;
    if ip_addr.is_multicast() {
        socket.join_multicast_v4(&ip_addr, &local_ip)?
    }
    Ok(socket)
}

pub(crate) fn create_ipv6_rx_socket(ip_addr: Ipv6Addr, port: u16) -> Result<Socket, RxError> {
    log::info!(
        "Creating IPv6 {} RX socket for stream at {}:{}",
        if ip_addr.is_multicast() {
            "multicast"
        } else {
            "unicast"
        },
        ip_addr,
        port
    );

    let local_ip = if ip_addr.is_multicast() {
        Ipv6Addr::UNSPECIFIED
    } else {
        ip_addr
    };
    let local_addr = SocketAddr::new(IpAddr::V6(local_ip), port);

    let socket = Socket::new(Domain::IPV6, Type::DGRAM, Some(Protocol::UDP))?;

    socket.set_reuse_address(true)?;
    // socket.set_reuse_port(true)?;
    socket.set_nonblocking(true)?;

    socket.bind(&SockAddr::from(local_addr))?;
    if ip_addr.is_multicast() {
        socket.join_multicast_v6(&ip_addr, 0)?
    }
    Ok(socket)
}
