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

mod jack;

pub(crate) use jack::JackAudioSystem;

use crate::utils::{MediaClockTimestamp, PlayoutBufferReader, PlayoutBufferWriter};
use tokio::sync::{mpsc, oneshot};

#[derive(Debug, Clone, Copy)]
pub(crate) struct RtpSample<S>(pub MediaClockTimestamp, pub S);

pub(crate) trait AudioSystem {
    type SampleFormat;

    fn close(&mut self);

    fn sample_rate(&self) -> usize;
}

pub(crate) type TransmitterBufferInitCallback<S> =
    mpsc::Sender<(usize, oneshot::Sender<Box<[mpsc::Receiver<S>]>>)>;

// pub(crate) type TransmitterBufferInitCallbackReceiver<S> =
//     mpsc::Receiver<(usize, oneshot::Sender<Box<[mpsc::Receiver<S>]>>)>;

pub(crate) type ReceiverBufferInitCallback<S> =
    mpsc::Sender<(usize, oneshot::Sender<Box<[mpsc::Receiver<RtpSample<S>>]>>)>;

// pub(crate) type ReceiverBufferInitCallbackReceiver<S> =
//     mpsc::Receiver<(usize, oneshot::Sender<Box<[mpsc::Receiver<S>]>>)>;

pub(crate) enum OutputEvent {
    BufferUnderrun(usize),
}

pub(crate) enum Message {
    ActiveInputsChanged(Box<[Option<(usize, PlayoutBufferWriter)>]>),
    ActiveOutputsChanged(Box<[Option<(usize, PlayoutBufferReader)>]>),
}
