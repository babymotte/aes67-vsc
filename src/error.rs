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
    ptp::PtpFunction,
    rtp::{rx::RxFunction, tx::TxFunction},
    sap::SapFunction,
};
use thiserror::Error;
use tokio::sync::{mpsc::error::SendError, oneshot::error::RecvError};

#[derive(Error, Debug)]
pub enum Aes67Error {
    #[error("error in transmitter: {0}")]
    TxError(#[from] TxError),
    #[error("error in receiver: {0}")]
    RxError(#[from] RxError),
    #[error("error in session announcement: {0}")]
    SapError(#[from] SapError),
    #[error("ptp error: {0}")]
    PtpError(#[from] PtpError),
}

#[derive(Error, Debug)]
pub enum TxError {
    #[error("io error: {0}")]
    IoError(#[from] io::Error),
    #[error("channel error: {0}")]
    SendError(#[from] SendError<TxFunction>),
    #[error("channel error: {0}")]
    ReceiveError(#[from] RecvError),
}

#[derive(Error, Debug)]
pub enum RxError {
    #[error("io error: {0}")]
    IoError(#[from] io::Error),
    #[error("channel error: {0}")]
    SendError(#[from] SendError<RxFunction>),
    #[error("channel error: {0}")]
    ReceiveError(#[from] RecvError),
}

#[derive(Error, Debug)]
pub enum SapError {
    #[error("sap-rs error: {0}")]
    SapRsError(#[from] sap_rs::error::Error),
    #[error("channel error: {0}")]
    SendError(#[from] SendError<SapFunction>),
    #[error("channel error: {0}")]
    ReceiveError(#[from] RecvError),
}

#[derive(Error, Debug)]
pub enum PtpError {
    #[error("io error: {0}")]
    IoError(#[from] io::Error),
    #[error("channel error: {0}")]
    SendError(#[from] SendError<PtpFunction>),
    #[error("channel error: {0}")]
    ReceiveError(#[from] RecvError),
}

pub type Aes67Result<T> = Result<T, Aes67Error>;
