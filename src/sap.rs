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
    error::{Aes67Error, Aes67Result, SapError},
};
use sap_rs::{Sap, SessionAnnouncement};
use sdp::SessionDescription;
use std::collections::HashMap;
use tokio::{
    select,
    sync::{mpsc, oneshot},
};
use tokio_graceful_shutdown::{SubsystemBuilder, SubsystemHandle};

pub struct SapApi {
    channel: mpsc::Sender<SapFunction>,
}

impl SapApi {
    pub fn new(subsys: &SubsystemHandle) -> Aes67Result<Self> {
        let (channel, commands) = mpsc::channel(1);

        subsys.start(SubsystemBuilder::new("sap", |s| async move {
            let token = s.create_cancellation_token();
            let mut actor = SapActor::new(s, commands);
            actor.run(token).await?;
            Ok(()) as Aes67Result<()>
        }));

        Ok(SapApi { channel })
    }

    pub async fn add_session(&self, sdp: SessionDescription) -> Aes67Result<String> {
        self.send_function(|tx| SapFunction::AddSession(sdp, tx))
            .await
    }

    pub async fn remove_session(&self, sdp: SessionDescription) -> Aes67Result<()> {
        self.send_function(|tx| SapFunction::RemoveSession(sdp, tx))
            .await
    }

    async fn send_function<T>(
        &self,
        function: impl FnOnce(oneshot::Sender<Aes67Result<T>>) -> SapFunction,
    ) -> Aes67Result<T> {
        actor::send_function::<T, SapFunction, SapError, Aes67Error>(
            function,
            &self.channel,
            SapError::SendError,
            SapError::ReceiveError,
        )
        .await
    }
}

#[derive(Debug)]
pub enum SapFunction {
    AddSession(SessionDescription, oneshot::Sender<Aes67Result<String>>),
    RemoveSession(SessionDescription, oneshot::Sender<Aes67Result<()>>),
}

struct SapActor {
    subsys: SubsystemHandle,
    sessions: HashMap<String, oneshot::Sender<()>>,
    commands: mpsc::Receiver<SapFunction>,
}

impl Actor<SapFunction, SapError> for SapActor {
    async fn recv_command(&mut self) -> Option<SapFunction> {
        self.commands.recv().await
    }

    async fn process_command(&mut self, command: Option<SapFunction>) -> Result<bool, SapError> {
        match command {
            Some(f) => match f {
                SapFunction::AddSession(sdp, tx) => {
                    respond(Ok(self.add_session(sdp).await?), tx).await
                }
                SapFunction::RemoveSession(sdp, tx) => {
                    respond(Ok(self.remove_session(sdp).await?), tx).await
                }
            },
            None => Ok(false),
        }
    }
}

impl SapActor {
    fn new(subsys: SubsystemHandle, commands: mpsc::Receiver<SapFunction>) -> Self {
        let sessions = HashMap::new();
        SapActor {
            sessions,
            subsys,
            commands,
        }
    }

    async fn add_session(&mut self, sdp: SessionDescription) -> Result<String, SapError> {
        let sap = Sap::new().await?;
        let session_id = format!("{}/{}", sdp.origin.session_id, sdp.origin.session_version);
        let session = Self::announce(sap, sdp, &self.subsys, &session_id).await?;
        self.sessions.insert(session_id.clone(), session);
        Ok(session_id)
    }

    async fn remove_session(&mut self, sdp: SessionDescription) -> Result<(), SapError> {
        let session_id = format!("{}/{}", sdp.origin.session_id, sdp.origin.session_version);
        if let Some(session) = self.sessions.remove(&session_id) {
            session.send(()).ok();
        }

        Ok(())
    }

    async fn announce(
        mut sap: Sap,
        sdp: SessionDescription,
        subsys: &SubsystemHandle,
        session_id: &str,
    ) -> Result<oneshot::Sender<()>, SapError> {
        let (tx, mut rx) = oneshot::channel();
        subsys.start(SubsystemBuilder::new(session_id, move |s| async move {
            let announcement = SessionAnnouncement::new(sdp)?;
            select! {
                _ = s.on_shutdown_requested() => (),
                _ = sap.announce_session(announcement) => (),
                _ = &mut rx => (),
            }

            sap.delete_session().await?;

            Ok(()) as Result<(), SapError>
        }));

        Ok(tx)
    }
}
