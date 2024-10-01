use crate::error::DiscoveryCleanupError;
use sdp::SessionDescription;
use std::{collections::HashMap, io::Cursor, net::IpAddr};
use tokio::select;
use tokio_graceful_shutdown::{SubsystemBuilder, SubsystemHandle};
use worterbuch_client::{topic, TypedKeyValuePair, TypedStateEvent, Worterbuch};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Session {
    ip: IpAddr,
    id: u64,
    version: u64,
}

enum SessionComp {
    Unrelated,
    Same,
    Newer,
    Older,
}

impl Session {
    fn from_kvp(kvp: &TypedKeyValuePair<String>) -> Option<Self> {
        if let Ok(sd) = SessionDescription::unmarshal(&mut Cursor::new(&kvp.value)) {
            if let Ok(ip) = sd.origin.unicast_address.parse() {
                Some(Session {
                    ip,
                    id: sd.origin.session_id,
                    version: sd.origin.session_version,
                })
            } else {
                None
            }
        } else {
            None
        }
    }

    fn compare(&self, other: &Session) -> SessionComp {
        if self.ip != other.ip || self.id != other.id {
            SessionComp::Unrelated
        } else if self.id > other.id {
            SessionComp::Newer
        } else if self.id < other.id {
            SessionComp::Older
        } else {
            SessionComp::Same
        }
    }
}

pub fn cleanup_discovery(susbsy: &SubsystemHandle, wb: Worterbuch, wb_root_key: String) {
    susbsy.start(SubsystemBuilder::new(
        "discovery-cleanup",
        move |s| async move {
            let (mut sub, _) = wb
                .psubscribe::<String>(
                    topic!(wb_root_key, "discovery", "?", "?", "?", "sdp"),
                    true,
                    false,
                    None,
                )
                .await?;

            let mut known_sessions: HashMap<String, Session> = HashMap::new();

            loop {
                select! {
                    _ = s.on_shutdown_requested() => break,
                    Some(es) = sub.recv() => {
                        for e in es {
                            match e {
                                TypedStateEvent::KeyValue(kvp) => {
                                    if let Some(session) = Session::from_kvp(&kvp) {
                                        for (k, sess) in known_sessions.clone() {
                                            match session.compare(&sess) {
                                                SessionComp::Unrelated => (),
                                                SessionComp::Same | SessionComp::Older => {
                                                    log::info!("Removing old discovery item {}", kvp.key);
                                                    wb.delete::<String>(k).await?;
                                                },
                                                SessionComp::Newer => {
                                                    log::info!("Removing old discovery item {}", k);
                                                    known_sessions.remove(&k);
                                                    wb.delete::<String>(k).await?;
                                                }
                                            }
                                        }
                                        known_sessions.insert(kvp.key, session);
                                    }
                                }
                                TypedStateEvent::Deleted(kvp) => {
                                    known_sessions.remove(&kvp.key);
                                }
                            }
                        }
                    },
                    else => break,
                }
            }

            Ok::<(), DiscoveryCleanupError>(())
        },
    ));
}
