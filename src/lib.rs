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

#[derive(Debug, Clone, PartialEq)]
pub enum MediaType {
    Audio,
    Video,
    Text,
    Application,
    Message,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Protocol {
    Udp,
    RtpAvp,
    RtpSavp,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Aes67SenderDescriptor {
    // o
    username: String,
    sess_id: String,
    sess_version: String,
    nettype: String,
    addrtype: String,
    unicast_address: String,
    // s
    session_name: String,
    // t
    start_time: u64,
    stop_time: u64,
    // m
    media: MediaType,
    port: u16,
    proto: Protocol,
}

#[derive(Debug, Clone, PartialEq)]
pub enum UiCommandTx {}

#[derive(Debug, Clone, PartialEq)]
pub enum UiCommandRx {
    Subscribe(Aes67SenderDescriptor),
}

#[derive(Debug, Clone, PartialEq)]
pub enum UiCommandPTP {
    SetPrio(u16),
}

#[derive(Debug, Clone, PartialEq)]
pub enum UiCommandSAP {}
