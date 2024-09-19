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
    actor::{self, Actor},
    error::{Aes67Error, Aes67Result, TxError},
};
use tokio::sync::{mpsc, oneshot};
use tokio_graceful_shutdown::{SubsystemBuilder, SubsystemHandle};

#[derive(Clone)]
pub struct RtpTxApi {
    channel: mpsc::Sender<TxFunction>,
}

impl RtpTxApi {
    pub fn new(subsys: &SubsystemHandle) -> Aes67Result<Self> {
        let (channel, commands) = mpsc::channel(1);

        subsys.start(SubsystemBuilder::new("rtp/tx", |s| async move {
            let mut actor = RtpTxActor::new(commands);
            actor.run(s.create_cancellation_token()).await?;
            Ok(()) as Aes67Result<()>
        }));

        Ok(RtpTxApi { channel })
    }

    pub async fn create_transmitter(&self, id: TransmitterId, channels: usize) -> Aes67Result<()> {
        self.send_function(|tx| TxFunction::CreateSender(TxDescriptor { channels, id }, tx))
            .await
    }

    pub async fn delete_transmitter(&self, id: TransmitterId) -> Aes67Result<()> {
        self.send_function(|tx| TxFunction::DeleteSender(id, tx))
            .await
    }

    async fn send_function<T>(
        &self,
        function: impl FnOnce(oneshot::Sender<Aes67Result<T>>) -> TxFunction,
    ) -> Aes67Result<T> {
        actor::send_function::<T, TxFunction, TxError, Aes67Error>(
            function,
            &self.channel,
            TxError::SendError,
            TxError::ReceiveError,
        )
        .await
    }
}

pub(crate) type TransmitterId = String;

#[derive(Debug, Clone, PartialEq, Hash)]
pub struct TxDescriptor {
    channels: usize,
    id: TransmitterId,
}

#[derive(Debug)]
pub enum TxFunction {
    CreateSender(TxDescriptor, oneshot::Sender<Aes67Result<()>>),
    DeleteSender(TransmitterId, oneshot::Sender<Aes67Result<()>>),
}

struct RtpTxActor {
    commands: mpsc::Receiver<TxFunction>,
}

impl Actor<TxFunction, TxError> for RtpTxActor {
    async fn recv_command(&mut self) -> Option<TxFunction> {
        self.commands.recv().await
    }

    async fn process_command(&mut self, command: Option<TxFunction>) -> Result<bool, TxError> {
        match command {
            Some(f) => {
                // TODO
                log::info!("{f:?}");
                Ok(true)
            }
            None => Ok(false),
        }
    }
}

impl RtpTxActor {
    fn new(commands: mpsc::Receiver<TxFunction>) -> Self {
        RtpTxActor { commands }
    }
}
