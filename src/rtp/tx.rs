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

use crate::{
    actor::{Actor, ActorApi},
    error::{RxError, TxError},
    ptp::statime_linux::SystemClock,
    TransmitterId,
};
use tokio::sync::{mpsc, oneshot};
use tokio_graceful_shutdown::{SubsystemBuilder, SubsystemHandle};

#[derive(Clone)]
pub struct RtpTxApi {
    channel: mpsc::Sender<TxFunction>,
}

impl ActorApi for RtpTxApi {
    type Message = TxFunction;
    type Error = TxError;

    fn message_tx(&self) -> &mpsc::Sender<Self::Message> {
        &self.channel
    }
}

impl RtpTxApi {
    pub fn new(subsys: &SubsystemHandle, clock: SystemClock) -> Result<Self, RxError> {
        let (channel, commands) = mpsc::channel(1);

        subsys.start(SubsystemBuilder::new("rtp/tx", |s| async move {
            let mut actor = RtpTxActor::new(commands);
            actor
                .run("rtp-rx".to_owned(), s.create_cancellation_token())
                .await
        }));

        Ok(RtpTxApi { channel })
    }

    pub async fn create_transmitter(
        &self,
        id: TransmitterId,
        channels: usize,
    ) -> Result<(), TxError> {
        self.send_message(|tx| TxFunction::CreateSender(TxDescriptor { channels, id }, tx))
            .await
    }

    pub async fn delete_transmitter(&self, id: TransmitterId) -> Result<(), TxError> {
        self.send_message(|tx| TxFunction::DeleteSender(id, tx))
            .await
    }
}

#[derive(Debug, Clone, PartialEq, Hash)]
pub struct TxDescriptor {
    channels: usize,
    id: TransmitterId,
}

#[derive(Debug)]
pub enum TxFunction {
    CreateSender(TxDescriptor, oneshot::Sender<Result<(), TxError>>),
    DeleteSender(TransmitterId, oneshot::Sender<Result<(), TxError>>),
}

struct RtpTxActor {
    commands: mpsc::Receiver<TxFunction>,
}

impl Actor for RtpTxActor {
    type Message = TxFunction;
    type Error = TxError;

    async fn recv_message(&mut self) -> Option<TxFunction> {
        self.commands.recv().await
    }

    async fn process_message(&mut self, command: TxFunction) -> bool {
        // TODO
        log::info!("{command:?}");
        true
    }
}

impl RtpTxActor {
    fn new(commands: mpsc::Receiver<TxFunction>) -> Self {
        RtpTxActor { commands }
    }
}
