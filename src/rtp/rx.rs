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
    error::{Aes67Error, Aes67Result, RxError},
};
use tokio::sync::{mpsc, oneshot};
use tokio_graceful_shutdown::{SubsystemBuilder, SubsystemHandle};

#[derive(Clone)]
pub struct RtpRxApi {
    channel: mpsc::Sender<RxFunction>,
}

impl RtpRxApi {
    pub fn new(subsys: &SubsystemHandle) -> Aes67Result<Self> {
        let (channel, commands) = mpsc::channel(1);

        subsys.start(SubsystemBuilder::new("rtp/rx", |s| async move {
            let mut actor = RtpRxActor::new(commands);
            actor.run(s.create_cancellation_token()).await?;
            Ok(()) as Aes67Result<()>
        }));

        Ok(RtpRxApi { channel })
    }

    pub async fn create_receiver(&self, id: ReceiverId, channels: usize) -> Aes67Result<()> {
        self.send_function(|tx| RxFunction::CreateReceiver(RxDescriptor { channels, id }, tx))
            .await
    }

    pub async fn delete_receiver(&self, id: ReceiverId) -> Aes67Result<()> {
        self.send_function(|tx| RxFunction::DeleteReceiver(id, tx))
            .await
    }

    async fn send_function<T>(
        &self,
        function: impl FnOnce(oneshot::Sender<Aes67Result<T>>) -> RxFunction,
    ) -> Aes67Result<T> {
        actor::send_function::<T, RxFunction, RxError, Aes67Error>(
            function,
            &self.channel,
            RxError::SendError,
            RxError::ReceiveError,
        )
        .await
    }
}

pub(crate) type ReceiverId = String;

#[derive(Debug, Clone, PartialEq, Hash)]
pub struct RxDescriptor {
    channels: usize,
    id: ReceiverId,
}

#[derive(Debug)]
pub enum RxFunction {
    CreateReceiver(RxDescriptor, oneshot::Sender<Aes67Result<()>>),
    DeleteReceiver(ReceiverId, oneshot::Sender<Aes67Result<()>>),
}

struct RtpRxActor {
    commands: mpsc::Receiver<RxFunction>,
}

impl Actor<RxFunction, RxError> for RtpRxActor {
    async fn recv_command(&mut self) -> Option<RxFunction> {
        self.commands.recv().await
    }

    async fn process_command(&mut self, command: Option<RxFunction>) -> Result<bool, RxError> {
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

impl RtpRxActor {
    fn new(commands: mpsc::Receiver<RxFunction>) -> Self {
        RtpRxActor { commands }
    }
}
