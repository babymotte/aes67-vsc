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

use std::io;

use crate::{
    rtp::{RxConfig, RxFunction, RxThreadFunction, TxFunction},
    sap::SapFunction,
    status::Status,
    ReceiverId,
};
use thiserror::Error;
use tokio::sync::{
    mpsc::error::{SendError, TrySendError},
    oneshot::error::RecvError,
};

#[derive(Error, Debug)]
pub enum Aes67Error {
    #[error("rtp error: {0}")]
    RtpError(#[from] RtpError),
    #[error("error in session announcement: {0}")]
    SapError(#[from] SapError),
    #[error("ptp error: {0}")]
    PtpError(#[from] PtpError),
    #[error("status error: {0}")]
    StatusError(#[from] StatusError),
    #[error("discovery cleanup: {0}")]
    DiscoveryCleanupError(#[from] DiscoveryCleanupError),
}

pub type Aes67Result<T> = Result<T, Aes67Error>;

#[derive(Error, Debug)]
pub enum RtpError {
    #[error("tx error: {0}")]
    TxError(#[from] TxError),
    #[error("rx error: {0}")]
    RxError(#[from] RxError),
    #[error("jack error: {0}")]
    JackError(#[from] jack::Error),
}

pub type RtpResult<T> = Result<T, RtpError>;

#[derive(Error, Debug)]
pub enum TxError {
    #[error("io error: {0}")]
    IoError(#[from] io::Error),
    #[error("channel error: {0}")]
    SendError(#[from] SendError<TxFunction>),
    #[error("channel error: {0}")]
    ReceiveError(#[from] RecvError),
}

pub type TxResult<T> = Result<T, TxError>;

#[derive(Error, Debug)]
pub enum RxError {
    #[error("io error: {0}")]
    IoError(#[from] io::Error),
    #[error("channel error: {0}")]
    SendError(#[from] SendError<RxFunction>),
    #[error("channel error: {0}")]
    ReceiveError(#[from] RecvError),
    #[error("channel error: {0}")]
    ThreadSendError(#[from] SendError<RxThreadFunction>),
    #[error("invalid sdp: {0}")]
    InvalidSdp(String),
    #[error("invalid receiver id: {0}")]
    InvalidReceiverId(ReceiverId),
    #[error("max channels exceeded: {0}")]
    MaxChannelsExceeded(usize),
    #[error("could not apply receiver config: {0}")]
    RxCfgSendError(#[from] SendError<RxConfig>),
    #[error("invalid link offset: {0} (max is {1})")]
    InvalidLinkOffset(f32, f32),
    #[error("playout device not found: {0}")]
    NoPlayoutDevice(String),
    #[error("status error: {0}")]
    StatusError(#[from] StatusError),
    #[error("error: {0}")]
    Other(String),
}

pub type RxResult<T> = Result<T, RxError>;

#[derive(Error, Debug)]
pub enum SapError {
    #[error("sap-rs error: {0}")]
    SapRsError(#[from] sap_rs::error::Error),
    #[error("channel error: {0}")]
    SendError(#[from] SendError<SapFunction>),
    #[error("channel error: {0}")]
    ReceiveError(#[from] RecvError),
    #[error("error in worterbuch connection: {0}")]
    WorterbuchError(#[from] worterbuch_client::ConnectionError),
}
pub type SapResult<T> = Result<T, SapError>;

#[derive(Error, Debug)]
pub enum PtpError {
    #[error("io error: {0}")]
    IoError(#[from] io::Error),
    #[error("channel error: {0}")]
    ReceiveError(#[from] RecvError),
    #[error("clock error: {0}")]
    ClockError(&'static str),
    #[error("unknown os error")]
    Unknown,
}

pub type PtpResult<T> = Result<T, PtpError>;

#[derive(Error, Debug)]
pub enum StatusError {
    #[error("error in worterbuch connection: {0}")]
    WorterbuchError(#[from] worterbuch_client::ConnectionError),
    #[error("channel error: {0}")]
    SendError(#[from] SendError<Status>),
    #[error("channel error: {0}")]
    TrySendError(#[from] TrySendError<Status>),
}

pub type StatusResult<T> = Result<T, StatusError>;

#[derive(Error, Debug)]
pub enum DiscoveryCleanupError {
    #[error("error in worterbuch connection: {0}")]
    WorterbuchError(#[from] worterbuch_client::ConnectionError),
}

pub type DiscoveryCleanupResult<T> = Result<T, DiscoveryCleanupError>;
