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
    actor::{self, respond, Actor},
    error::{Aes67Error, Aes67Result, PtpError},
};
use miette::Result;
use tokio::sync::{mpsc, oneshot};
use tokio_graceful_shutdown::{SubsystemBuilder, SubsystemHandle};

pub struct PtpApi {
    channel: mpsc::Sender<PtpFunction>,
}

impl PtpApi {
    pub fn new(subsys: &SubsystemHandle) -> Aes67Result<Self> {
        let (channel, commands) = mpsc::channel(1);

        subsys.start(SubsystemBuilder::new("ptp", |s| async move {
            let mut actor = PtpActor::new(commands);
            actor.run(s.create_cancellation_token()).await?;
            Ok(()) as Aes67Result<()>
        }));

        Ok(PtpApi { channel })
    }

    pub async fn set_prio(&self, prio: u16) -> Aes67Result<()> {
        self.send_function(|tx| PtpFunction::SetPrio(prio, tx))
            .await
    }

    async fn send_function<T>(
        &self,
        function: impl FnOnce(oneshot::Sender<Aes67Result<T>>) -> PtpFunction,
    ) -> Aes67Result<T> {
        actor::send_function::<T, PtpFunction, PtpError, Aes67Error>(
            function,
            &self.channel,
            PtpError::SendError,
            PtpError::ReceiveError,
        )
        .await
    }
}

#[derive(Debug)]
pub enum PtpFunction {
    SetPrio(u16, oneshot::Sender<Aes67Result<()>>),
}

struct PtpActor {
    commands: mpsc::Receiver<PtpFunction>,
}

impl Actor<PtpFunction, PtpError> for PtpActor {
    async fn recv_command(&mut self) -> Option<PtpFunction> {
        self.commands.recv().await
    }

    async fn process_command(&mut self, command: Option<PtpFunction>) -> Result<bool, PtpError> {
        match command {
            Some(f) => match f {
                PtpFunction::SetPrio(prio, tx) => respond(Ok(self.set_prio(prio).await?), tx).await,
            },
            None => Ok(false),
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
}
