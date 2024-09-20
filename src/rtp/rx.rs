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
    error::RxError,
};
use tokio::sync::{mpsc, oneshot};
use tokio_graceful_shutdown::{SubsystemBuilder, SubsystemHandle};

#[derive(Clone)]
pub struct RtpRxApi {
    channel: mpsc::Sender<RxFunction>,
}

impl ActorApi for RtpRxApi {
    type Message = RxFunction;
    type Error = RxError;

    fn message_tx(&self) -> &mpsc::Sender<RxFunction> {
        &self.channel
    }
}

impl RtpRxApi {
    pub fn new(subsys: &SubsystemHandle) -> Result<Self, RxError> {
        let (channel, commands) = mpsc::channel(1);

        subsys.start(SubsystemBuilder::new("rtp/rx", |s| async move {
            let mut actor = RtpRxActor::new(commands);
            actor.run(s.create_cancellation_token()).await
        }));

        Ok(RtpRxApi { channel })
    }

    pub async fn create_receiver(&self, id: ReceiverId, channels: usize) -> Result<(), RxError> {
        self.send_message(|tx| RxFunction::CreateReceiver(RxDescriptor { channels, id }, tx))
            .await
    }

    pub async fn delete_receiver(&self, id: ReceiverId) -> Result<(), RxError> {
        self.send_message(|tx| RxFunction::DeleteReceiver(id, tx))
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
    CreateReceiver(RxDescriptor, oneshot::Sender<Result<(), RxError>>),
    DeleteReceiver(ReceiverId, oneshot::Sender<Result<(), RxError>>),
}

struct RtpRxActor {
    commands: mpsc::Receiver<RxFunction>,
}

impl Actor for RtpRxActor {
    type Message = RxFunction;
    type Error = RxError;

    async fn recv_message(&mut self) -> Option<RxFunction> {
        self.commands.recv().await
    }

    async fn process_message(&mut self, command: RxFunction) -> bool {
        // TODO
        log::info!("{command:?}");
        true
    }
}

impl RtpRxActor {
    fn new(commands: mpsc::Receiver<RxFunction>) -> Self {
        RtpRxActor { commands }
    }
}
