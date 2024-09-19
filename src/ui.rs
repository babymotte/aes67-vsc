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

use std::{env, io::Cursor};

use crate::actor::{self, respond};
use aes67_vsc::{
    ptp::PtpApi,
    rtp::{rx::RtpRxApi, tx::RtpTxApi},
    sap::SapApi,
};
use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use miette::{miette, Error, IntoDiagnostic, Result};
use pnet::{datalink, ipnetwork::IpNetwork};
use sdp::SessionDescription;
use serde::{Deserialize, Serialize};
use tokio::{
    select,
    sync::{mpsc, oneshot},
};
use tokio_graceful_shutdown::{SubsystemBuilder, SubsystemHandle};
use tower_http::services::{ServeDir, ServeFile};

struct UiActor {
    subsys: SubsystemHandle,
    rtp_tx: RtpTxApi,
    rtp_rx: RtpRxApi,
    ptp: PtpApi,
    sap: SapApi,
    ui_commands: mpsc::Receiver<UiFunction>,
}

impl UiActor {
    async fn run(mut self) -> Result<()> {
        loop {
            select! {
                _ = self.subsys.on_shutdown_requested() => break,
                recv = self.ui_commands.recv() => {
                    if !self.process_command(recv).await? {
                        break;
                    }
                }
            }
        }

        Ok(())
    }

    async fn process_command(&mut self, recv: Option<UiFunction>) -> Result<bool> {
        match recv {
            Some(f) => match f {
                UiFunction::CreateTransmitter(sdp, tx) => {
                    respond(self.create_transmitter(sdp).await, tx).await
                }
            },
            None => Ok(false),
        }
    }

    async fn create_transmitter(&self, sdp: String) -> Result<String> {
        let sd = SessionDescription::unmarshal(&mut Cursor::new(sdp)).into_diagnostic()?;
        let session_id = self.sap.add_session(sd).await.into_diagnostic()?;

        Ok(session_id)
    }
}

pub fn ui(
    subsys: &SubsystemHandle,
    rtp_tx: RtpTxApi,
    rtp_rx: RtpRxApi,
    ptp: PtpApi,
    sap: SapApi,
    port: u16,
) -> Result<()> {
    subsys.start(SubsystemBuilder::new("ui", move |s| async move {
        let webapp_root_dir =
            env::var("AES67_VSC_WEBAPP_ROOT_DIR").unwrap_or("web-ui/dist".to_owned());

        for iface in datalink::interfaces() {
            for ip in iface.ips {
                if let IpNetwork::V4(ip) = ip {
                    log::info!("WebUI: http://{}:{}", ip.ip(), port);
                }
            }
        }

        let (channel, ui_commands) = mpsc::channel(1000);
        let ui_api = UiApi { channel };

        s.start(SubsystemBuilder::new("web-server", move |s| async move {
            let serve_dir = ServeDir::new(&webapp_root_dir)
                .not_found_service(ServeFile::new(format!("{webapp_root_dir}/index.html")));

            let app = Router::new()
                .route("/api/v1/create/transmitter", post(create_transmitter))
                .route("/api/v1/wb/config", get(wb_config))
                .nest_service("/", serve_dir.clone())
                .fallback_service(serve_dir)
                .with_state(ui_api);

            let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}"))
                .await
                .unwrap();
            axum::serve(listener, app)
                .with_graceful_shutdown(async move { s.on_shutdown_requested().await })
                .await
                .unwrap();

            Ok(()) as Result<()>
        }));

        let actor = UiActor {
            ptp,
            rtp_rx,
            rtp_tx,
            sap,
            subsys: s,
            ui_commands,
        };

        actor.run().await?;

        Ok(()) as Result<()>
    }));

    Ok(())
}

#[derive(Debug)]
enum UiFunction {
    CreateTransmitter(String, oneshot::Sender<Result<String>>),
}

#[derive(Debug, Clone)]
struct UiApi {
    channel: mpsc::Sender<UiFunction>,
}

impl UiApi {
    async fn create_transmitter(&self, sdp: String) -> Result<String> {
        self.send_function(|tx| UiFunction::CreateTransmitter(sdp, tx))
            .await
    }

    async fn send_function<T>(
        &self,
        function: impl FnOnce(oneshot::Sender<Result<T>>) -> UiFunction,
    ) -> Result<T> {
        actor::send_function::<T, UiFunction, Error, Error>(
            function,
            &self.channel,
            |e| miette!(e),
            |e| miette!(e),
        )
        .await
    }
}

async fn create_transmitter(
    State(ui_api): State<UiApi>,
    Json(payload): Json<CreateTransmitter>,
) -> impl IntoResponse {
    let session_id = match ui_api.create_transmitter(payload.sdp).await {
        Ok(it) => Some(it),
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(TransmitterCreated {
                    session_id: None,
                    error: Some(e.to_string()),
                }),
            )
        }
    };

    (
        StatusCode::CREATED,
        Json(TransmitterCreated {
            session_id,
            error: None,
        }),
    )
}

async fn wb_config() -> impl IntoResponse {
    // TODO load from config
    Json(WorterbuchConfig {
        backend_scheme: "ws".to_owned(),
        backend_host: "localhost".to_owned(),
        backend_port: Some(8080),
        backend_path: "/ws".to_owned(),
        backend_auth_token: None,
    })
}

#[derive(Deserialize)]
struct CreateTransmitter {
    sdp: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TransmitterCreated {
    session_id: Option<String>,
    error: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WorterbuchConfig {
    backend_scheme: String,
    backend_host: String,
    backend_port: Option<u16>,
    backend_path: String,
    backend_auth_token: Option<String>,
}
