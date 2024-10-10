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
    error::{StatusError, StatusResult},
    rtp::{Channel, RxDescriptor},
    utils::MediaClockTimestamp,
    ReceiverId,
};
use serde_json::json;
use std::net::SocketAddr;
use tokio::sync::mpsc;
use tokio_graceful_shutdown::{SubsystemBuilder, SubsystemHandle};
use worterbuch_client::{topic, Value, Worterbuch};

#[derive(Debug, Clone)]
pub enum Status {
    UsedInputChannels(usize, usize),
    UsedOutputChannels(usize, usize),
    Receiver(Receiver),
    Output(Output),
}

pub enum Transmitter {
    Sdp(ReceiverId, String),
}

#[derive(Debug, Clone)]
pub enum Receiver {
    Created(RxDescriptor, Option<String>),
    Deleted(RxDescriptor),
    LinkOffset(ReceiverId, usize),
    MulticastAddress(ReceiverId, SocketAddr),
    PacketTime(ReceiverId, usize),
    SessionIdAndVersion(ReceiverId, u64, u64),
    SessionName(ReceiverId, String),
    Sdp(ReceiverId, String),
    Meter(ReceiverId, usize, u16),
    BufferUnderrun(Channel, MediaClockTimestamp),
    BufferOverflow(Channel, MediaClockTimestamp),
    BufferUsage(ReceiverId, usize, usize, usize),
    DroppedPackets(ReceiverId, usize),
    OutOfOrderPackets(ReceiverId, usize),
}

#[derive(Debug, Clone)]
pub enum Output {
    Meter(usize, usize, u16),
    BufferUnderrun(usize, MediaClockTimestamp),
    BufferOverflow(usize, MediaClockTimestamp),
}

#[derive(Clone)]
pub struct StatusApi {
    channel: mpsc::Sender<Status>,
}

impl ActorApi for StatusApi {
    type Message = Status;
    type Error = StatusError;

    fn message_tx(&self) -> &mpsc::Sender<Self::Message> {
        &self.channel
    }
}

impl StatusApi {
    pub fn new(subsys: &SubsystemHandle, wb: Worterbuch, root_key: String) -> StatusResult<Self> {
        let (channel, commands) = mpsc::channel(1000);

        subsys.start(SubsystemBuilder::new("status", |s| async move {
            let mut actor = StatusActor::new(commands, wb, root_key);
            actor
                .run("status".to_owned(), s.create_cancellation_token())
                .await
        }));

        Ok(StatusApi { channel })
    }

    pub async fn publish(&self, status: Status) -> StatusResult<()> {
        Ok(self.channel.send(status).await?)
    }

    pub fn publish_blocking(&self, status: Status) -> StatusResult<()> {
        Ok(self.channel.blocking_send(status)?)
    }

    pub fn try_publish(&self, status: Status) -> StatusResult<()> {
        Ok(self.channel.try_send(status)?)
    }
}

struct StatusActor {
    commands: mpsc::Receiver<Status>,
    wb: Worterbuch,
    root_key: String,
}

impl Actor for StatusActor {
    type Message = Status;
    type Error = StatusError;

    async fn recv_message(&mut self) -> Option<Status> {
        self.commands.recv().await
    }

    async fn process_message(&mut self, status: Status) -> bool {
        let action = self.to_action(status);
        match action {
            Action::Set(kvps) => {
                for (key, value) in kvps {
                    if self
                        .wb
                        .set(topic!(self.root_key, key), value)
                        .await
                        .is_err()
                    {
                        return false;
                    }
                }
            }
            Action::Publish(kvps) => {
                for (key, value) in kvps {
                    if self
                        .wb
                        .publish(topic!(self.root_key, key), &value)
                        .await
                        .is_err()
                    {
                        return false;
                    }
                }
            }
            Action::Delete(keys) => {
                for key in keys {
                    if self
                        .wb
                        .pdelete::<Value>(topic!(self.root_key, key), true)
                        .await
                        .is_err()
                    {
                        return false;
                    }
                }
            }
        }
        true
    }
}

enum Action {
    Set(Vec<(String, Value)>),
    Publish(Vec<(String, Value)>),
    Delete(Vec<String>),
}

impl StatusActor {
    fn new(commands: mpsc::Receiver<Status>, wb: Worterbuch, root_key: String) -> Self {
        Self {
            commands,
            wb,
            root_key,
        }
    }

    fn to_action(&self, status: Status) -> Action {
        match status {
            Status::UsedInputChannels(used, max) => Action::Set(vec![
                (topic!("resources", "inputChannels", "used"), json!(used)),
                (topic!("resources", "inputChannels", "max"), json!(max)),
            ]),
            Status::UsedOutputChannels(used, max) => Action::Set(vec![
                (topic!("resources", "outputChannels", "used"), json!(used)),
                (topic!("resources", "outputChannels", "max"), json!(max)),
            ]),
            Status::Receiver(receiver) => match receiver {
                Receiver::Created(desc, sdp) => {
                    let mut vec = vec![
                        (
                            topic!("receivers", desc.id, "config", "sampleFormat"),
                            json!(desc.sample_format),
                        ),
                        (
                            topic!("receivers", desc.id, "config", "channels"),
                            json!(desc.channels),
                        ),
                        (
                            topic!("receivers", desc.id, "config", "packetTimeMs"),
                            json!(desc.packet_time),
                        ),
                        (
                            topic!("receivers", desc.id, "config", "sampleRate"),
                            json!(desc.sample_rate),
                        ),
                        (
                            topic!("receivers", desc.id, "config", "session", "id"),
                            json!(desc.session_id),
                        ),
                        (
                            topic!("receivers", desc.id, "config", "session", "version"),
                            json!(desc.session_version),
                        ),
                        (
                            topic!("receivers", desc.id, "config", "session", "name"),
                            json!(desc.session_name),
                        ),
                        (
                            topic!("sessions", desc.session_id, desc.session_version, "name"),
                            json!(desc.session_name),
                        ),
                        (
                            topic!(
                                "sessions",
                                desc.session_id,
                                desc.session_version,
                                "receiver"
                            ),
                            json!(desc.id),
                        ),
                    ];
                    if let Some(sdp) = sdp {
                        vec.push((topic!("receivers", desc.id, "config", "sdp"), json!(sdp)));
                    }
                    Action::Set(vec)
                }
                Receiver::Deleted(desc) => Action::Delete(vec![
                    topic!("receivers", desc.id, "#"),
                    topic!("sessions", desc.session_id, "#"),
                ]),
                Receiver::LinkOffset(id, value) => Action::Set(vec![(
                    topic!("receivers", id, "linkOffsetMs"),
                    json!(value),
                )]),
                Receiver::MulticastAddress(id, value) => Action::Set(vec![(
                    topic!("receivers", id, "multicastAddress"),
                    json!(value),
                )]),
                Receiver::PacketTime(id, value) => Action::Set(vec![(
                    topic!("receivers", id, "packetTimeMs"),
                    json!(value),
                )]),
                Receiver::SessionIdAndVersion(id, value1, value2) => Action::Set(vec![
                    (topic!("receivers", id, "session", "id"), json!(value1)),
                    (topic!("receivers", id, "session", "version"), json!(value2)),
                ]),
                Receiver::SessionName(id, value) => Action::Set(vec![(
                    topic!("receivers", id, "session", "name"),
                    json!(value),
                )]),
                Receiver::Sdp(id, value) => {
                    Action::Set(vec![(topic!("receivers", id, "sdp"), json!(value))])
                }
                Receiver::Meter(id, channel, value) => Action::Publish(vec![(
                    topic!("receivers", id, "meter", channel),
                    json!(value),
                )]),
                Receiver::BufferUnderrun(id, value) => Action::Publish(vec![(
                    topic!(
                        "receivers",
                        id.transceiver_id,
                        "buffer",
                        "underrun",
                        id.channel_nr
                    ),
                    json!(value.timestamp),
                )]),
                Receiver::BufferOverflow(id, value) => Action::Publish(vec![(
                    topic!(
                        "receivers",
                        id.transceiver_id,
                        "buffer",
                        "overflow",
                        id.channel_nr
                    ),
                    json!(value.timestamp),
                )]),
                Receiver::BufferUsage(id, used, size, percent) => Action::Publish(vec![
                    (topic!("receivers", id, "buffer", "size"), json!(size)),
                    (topic!("receivers", id, "buffer", "used"), json!(used)),
                    (
                        topic!("receivers", id, "buffer", "used", "percent"),
                        json!(percent),
                    ),
                ]),
                Receiver::DroppedPackets(id, count) => Action::Publish(vec![(
                    topic!("receivers", id, "packets", "dropped"),
                    json!(count),
                )]),
                Receiver::OutOfOrderPackets(id, count) => Action::Publish(vec![(
                    topic!("receivers", id, "packets", "outOfOrder"),
                    json!(count),
                )]),
            },
            Status::Output(output) => match output {
                Output::Meter(id, channel, value) => Action::Publish(vec![(
                    topic!("receivers", id, "meter", channel),
                    json!(value),
                )]),
                Output::BufferUnderrun(channel, value) => Action::Publish(vec![(
                    topic!("outputs", channel, "buffer", "underrun"),
                    json!(value.timestamp),
                )]),
                Output::BufferOverflow(channel, value) => Action::Publish(vec![(
                    topic!("outputs", channel, "buffer", "overflow"),
                    json!(value.timestamp),
                )]),
            },
        }
    }
}
