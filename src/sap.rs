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
    error::SapError,
};
use sap_rs::{Sap, SessionAnnouncement};
use sdp::SessionDescription;
use std::collections::HashMap;
use tokio::{
    select,
    sync::{mpsc, oneshot},
};
use tokio_graceful_shutdown::{SubsystemBuilder, SubsystemHandle};
use worterbuch_client::{topic, Worterbuch};

#[derive(Debug, Clone)]
pub struct SapApi {
    channel: mpsc::Sender<SapFunction>,
}

impl ActorApi for SapApi {
    type Message = SapFunction;
    type Error = SapError;

    fn message_tx(&self) -> &mpsc::Sender<SapFunction> {
        &self.channel
    }
}

impl SapApi {
    pub fn new(
        subsys: &SubsystemHandle,
        wb: Worterbuch,
        root_key: String,
    ) -> Result<Self, SapError> {
        let (channel, messages) = mpsc::channel(1);

        subsys.start(SubsystemBuilder::new("sap", |s| async move {
            let token = s.create_cancellation_token();
            let mut actor = SapActor::new(s, messages);
            actor.start_discovery(wb.clone(), root_key).await?;
            actor.run("sap".to_owned(), token).await
        }));

        Ok(SapApi { channel })
    }

    pub async fn add_session(&self, sdp: SessionDescription) -> Result<String, SapError> {
        self.send_message(|tx| SapFunction::AddSession(sdp, tx))
            .await
    }

    pub async fn remove_session(&self, sdp: SessionDescription) -> Result<(), SapError> {
        self.send_message(|tx| SapFunction::RemoveSession(sdp, tx))
            .await
    }
}

#[derive(Debug)]
pub enum SapFunction {
    AddSession(
        SessionDescription,
        oneshot::Sender<Result<String, SapError>>,
    ),
    RemoveSession(SessionDescription, oneshot::Sender<Result<(), SapError>>),
}

struct SapActor {
    subsys: SubsystemHandle,
    sessions: HashMap<String, oneshot::Sender<()>>,
    messages: mpsc::Receiver<SapFunction>,
}

impl Actor for SapActor {
    type Message = SapFunction;
    type Error = SapError;

    async fn recv_message(&mut self) -> Option<SapFunction> {
        self.messages.recv().await
    }

    async fn process_message(&mut self, msg: SapFunction) -> bool {
        match msg {
            SapFunction::AddSession(sdp, tx) => respond(self.add_session(sdp), tx).await,
            SapFunction::RemoveSession(sdp, tx) => respond(self.remove_session(sdp), tx).await,
        }
    }
}

impl SapActor {
    fn new(subsys: SubsystemHandle, messages: mpsc::Receiver<SapFunction>) -> Self {
        let sessions = HashMap::new();
        SapActor {
            sessions,
            subsys,
            messages,
        }
    }

    async fn start_discovery(&mut self, wb: Worterbuch, root_key: String) -> Result<(), SapError> {
        log::info!("Starting SAP discovery …");
        let sap = Sap::new().await?;
        self.subsys
            .start(SubsystemBuilder::new("discovery", move |s| async move {
                let mut discovery = sap.discover_sessions().await;
                log::info!("SAP discovery running.");
                loop {
                    select! {
                        _ = s.on_shutdown_requested() => break,
                        Some(session) = discovery.recv() => {
                            match session {
                                Ok(sa) => {
                                    // TODO use customized key?
                                    log::debug!("Received SAP announcement …");
                                    let key = topic!(root_key, "discovery", "sap", sa.originating_source.to_string(), sa.msg_id_hash, "sdp");
                                    if sa.deletion {
                                        log::debug!("SDP {} was deleted by {}.", sa.msg_id_hash, sa.originating_source);
                                        wb.delete::<String>(key).await?;
                                    } else {
                                        let sdp = sa.sdp.marshal();
                                        log::debug!("SDP {} was announced by {}:\n{}", sa.msg_id_hash, sa.originating_source, sdp);
                                        wb.set(key, sdp).await?;
                                    }
                                },
                                Err(e) => {
                                    log::warn!("Received invalid session announcement: {e}");
                                }
                            }
                        },
                        else => break,
                    }
                }
                Ok::<(), SapError>(())
            }));

        Ok(())
    }

    async fn add_session(&mut self, sdp: SessionDescription) -> Result<String, SapError> {
        let session_id = format!("{} {}", sdp.origin.session_id, sdp.origin.session_version);
        log::info!("Adding session {session_id} …");
        let sap = Sap::new().await?;
        let session = Self::announce(sap, sdp, &self.subsys, &session_id).await?;
        self.sessions.insert(session_id.clone(), session);
        log::info!("Session {session_id} added.");
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
