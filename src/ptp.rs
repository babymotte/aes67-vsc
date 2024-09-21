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
    actor::{respond, Actor, ActorApi},
    error::PtpError,
};
use miette::Result;
use tokio::sync::{mpsc, oneshot};
use tokio_graceful_shutdown::{SubsystemBuilder, SubsystemHandle};

pub struct PtpApi {
    channel: mpsc::Sender<PtpFunction>,
}

impl ActorApi for PtpApi {
    type Message = PtpFunction;
    type Error = PtpError;

    fn message_tx(&self) -> &mpsc::Sender<PtpFunction> {
        &self.channel
    }
}

impl PtpApi {
    pub fn new(subsys: &SubsystemHandle) -> Result<Self, PtpError> {
        let (channel, commands) = mpsc::channel(1);

        subsys.start(SubsystemBuilder::new("ptp", |s| async move {
            let mut actor = PtpActor::new(commands);
            actor.run(s.create_cancellation_token()).await
        }));

        Ok(PtpApi { channel })
    }

    pub async fn set_prio(&self, prio: u16) -> Result<(), PtpError> {
        self.send_message(|tx| PtpFunction::SetPrio(prio, tx)).await
    }
}

#[derive(Debug)]
pub enum PtpFunction {
    SetPrio(u16, oneshot::Sender<Result<(), PtpError>>),
    StartClock(oneshot::Sender<Result<(), PtpError>>),
}

struct PtpActor {
    commands: mpsc::Receiver<PtpFunction>,
}

impl Actor for PtpActor {
    type Message = PtpFunction;
    type Error = PtpError;

    async fn recv_message(&mut self) -> Option<PtpFunction> {
        self.commands.recv().await
    }

    async fn process_message(&mut self, command: PtpFunction) -> bool {
        match command {
            PtpFunction::SetPrio(prio, tx) => respond(self.set_prio(prio), tx).await,
            PtpFunction::StartClock(tx) => respond(self.start_clock(), tx).await,
        }
    }
}

impl PtpActor {
    fn new(commands: mpsc::Receiver<PtpFunction>) -> Self {
        PtpActor { commands }
    }

    async fn set_prio(&mut self, _prio: u16) -> Result<(), PtpError> {
        // TODO
        Ok(())
    }

    async fn start_clock(&mut self) -> Result<(), PtpError> {
        // TODO
        Ok(())
    }
}
