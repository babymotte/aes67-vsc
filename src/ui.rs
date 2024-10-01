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

use crate::actor::{respond, Actor, ActorApi};
use aes67_vsc::{
    error::{RxError, SapError},
    ptp::PtpApi,
    rtp::{RtpRxApi, RtpTxApi},
    sap::SapApi,
    utils::open_browser,
};
use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use miette::{IntoDiagnostic, Result};
use pnet::{datalink, ipnetwork::IpNetwork};
use sdp::SessionDescription;
use serde::{Deserialize, Serialize};
use std::{env, io::Cursor, net::IpAddr};
use thiserror::Error;
use tokio::sync::{
    mpsc::{self, error::SendError},
    oneshot::{self, error::RecvError},
};
use tokio_graceful_shutdown::{SubsystemBuilder, SubsystemHandle};
use tower_http::{
    services::{ServeDir, ServeFile},
    trace::TraceLayer,
};
use worterbuch_client::{config::Config, Worterbuch};

#[derive(Error, Debug)]
enum UiError {
    #[error("channel error: {0}")]
    SendError(#[from] SendError<UiFunction>),
    #[error("channel error: {0}")]
    ReceiveError(#[from] RecvError),
    #[error("sap error: {0}")]
    SapError(#[from] SapError),
    #[error("sdp error: {0}")]
    SdpError(#[from] sdp::Error),
    #[error("receiver error: {0}")]
    RxError(#[from] RxError),
}

struct UiActor {
    subsys: SubsystemHandle,
    rtp_tx: RtpTxApi,
    rtp_rx: RtpRxApi,
    ptp: PtpApi,
    sap: SapApi,
    ui_commands: mpsc::Receiver<UiFunction>,
    wb: Worterbuch,
}

impl Actor for UiActor {
    type Message = UiFunction;
    type Error = UiError;

    async fn recv_message(&mut self) -> Option<UiFunction> {
        self.ui_commands.recv().await
    }

    async fn process_message(&mut self, command: UiFunction) -> bool {
        match command {
            UiFunction::CreateTransmitter(sdp, tx) => {
                respond(self.create_transmitter(sdp), tx).await
            }
            UiFunction::ReceiveStream(sdp, tx) => respond(self.receive_stream(sdp), tx).await,
            UiFunction::DeleteReceiver(receiver, tx) => {
                respond(self.delete_receiver(receiver), tx).await
            }
        }
    }
}

impl UiActor {
    async fn create_transmitter(&self, sdp: String) -> Result<String, UiError> {
        let sd = SessionDescription::unmarshal(&mut Cursor::new(sdp))?;
        let session_id = self.sap.add_session(sd).await?;
        Ok(session_id)
    }

    async fn receive_stream(&self, sdp: String) -> Result<(), UiError> {
        let sd = SessionDescription::unmarshal(&mut Cursor::new(sdp))?;
        self.rtp_rx.create_receiver(sd).await?;
        Ok(())
    }

    async fn delete_receiver(&self, receiver: usize) -> Result<(), UiError> {
        self.rtp_rx.delete_receiver(receiver).await?;
        Ok(())
    }
}

pub async fn ui(
    subsys: &SubsystemHandle,
    rtp_tx: RtpTxApi,
    rtp_rx: RtpRxApi,
    ptp: PtpApi,
    sap: SapApi,
    port: u16,
    wb: Worterbuch,
    wb_cfg: Config,
) -> Result<()> {
    subsys.start(SubsystemBuilder::new("ui", move |s| async move {
        let webapp_root_dir =
            env::var("AES67_VSC_WEBAPP_ROOT_DIR").unwrap_or("web-ui/dist".to_owned());

        for iface in datalink::interfaces() {
            for ip in iface.ips {
                if let IpNetwork::V4(ip) = ip {
                    log::info!("WebUI: http://{}:{}", ip.ip(), port);
                    if ip.ip().is_loopback() {
                        open_browser(IpAddr::V4(ip.ip()), port).await;
                    }
                }
            }
        }

        let (channel, ui_commands) = mpsc::channel(1000);
        let ui_api = UiApi { channel, wb_cfg };

        s.start(SubsystemBuilder::new("web-server", move |s| async move {
            let serve_dir = ServeDir::new(&webapp_root_dir)
                .not_found_service(ServeFile::new(format!("{webapp_root_dir}/index.html")));

            let app = Router::new()
                .route("/api/v1/create/transmitter", post(create_transmitter))
                .route("/api/v1/receive/stream", post(receive_stream))
                .route("/api/v1/delete/receiver", post(delete_receiver))
                .route("/api/v1/wb/config", get(wb_config))
                .nest_service("/", serve_dir.clone())
                .fallback_service(serve_dir)
                .layer(TraceLayer::new_for_http())
                .with_state(ui_api);

            let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}"))
                .await
                .into_diagnostic()?;
            axum::serve(listener, app)
                .with_graceful_shutdown(async move { s.on_shutdown_requested().await })
                .await
                .into_diagnostic()?;

            Ok(()) as Result<()>
        }));

        let cancel_token = s.create_cancellation_token();

        let mut actor = UiActor {
            ptp,
            rtp_rx,
            rtp_tx,
            sap,
            subsys: s,
            ui_commands,
            wb,
        };

        actor
            .run("ui".to_owned(), cancel_token)
            .await
            .into_diagnostic()?;
        actor.subsys.request_shutdown();

        Ok(()) as Result<()>
    }));

    Ok(())
}

#[derive(Debug)]
enum UiFunction {
    CreateTransmitter(String, oneshot::Sender<Result<String, UiError>>),
    ReceiveStream(String, oneshot::Sender<Result<(), UiError>>),
    DeleteReceiver(usize, oneshot::Sender<Result<(), UiError>>),
}

#[derive(Debug, Clone)]
struct UiApi {
    channel: mpsc::Sender<UiFunction>,
    wb_cfg: Config,
}

impl ActorApi for UiApi {
    type Message = UiFunction;
    type Error = UiError;

    fn message_tx(&self) -> &mpsc::Sender<Self::Message> {
        &self.channel
    }
}

impl UiApi {
    async fn create_transmitter(&self, sdp: String) -> Result<String, UiError> {
        self.send_message(|tx| UiFunction::CreateTransmitter(sdp, tx))
            .await
    }

    async fn receive_stream(&self, sdp: String) -> Result<(), UiError> {
        self.send_message(|tx| UiFunction::ReceiveStream(sdp, tx))
            .await
    }

    async fn delete_receiver(&self, receiver: usize) -> Result<(), UiError> {
        self.send_message(|tx| UiFunction::DeleteReceiver(receiver, tx))
            .await
    }

    async fn wb_config_for_web(&self) -> WorterbuchConfig {
        let Config {
            host_addr,
            auth_token,
            ..
        } = self.wb_cfg.clone();

        WorterbuchConfig {
            backend_scheme: "ws".to_owned(),
            backend_host: host_addr,
            // TODO use port from config
            backend_port: Some(80),
            backend_path: "/ws".to_owned(),
            backend_auth_token: auth_token,
        }
    }
}

async fn create_transmitter(
    State(ui_api): State<UiApi>,
    Json(payload): Json<CreateTransmitter>,
) -> impl IntoResponse {
    let session_id = match ui_api.create_transmitter(payload.sdp).await {
        Ok(it) => Some(it),
        Err(e) => {
            log::error!("Transmitter creation failed: {e}");
            return (
                StatusCode::BAD_REQUEST,
                Json(TransmitterCreated {
                    session_id: None,
                    error: Some(e.to_string()),
                }),
            );
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

async fn receive_stream(
    State(ui_api): State<UiApi>,
    Json(payload): Json<ReceiveStream>,
) -> impl IntoResponse {
    if let Err(e) = ui_api.receive_stream(payload.sdp).await {
        log::error!("Receiver creation failed: {e}");
        return (
            StatusCode::BAD_REQUEST,
            Json(ReceivingStream {
                result: None,
                error: Some(e.to_string()),
            }),
        );
    }

    (
        StatusCode::CREATED,
        Json(ReceivingStream {
            result: Some("OK".to_owned()),
            error: None,
        }),
    )
}

async fn delete_receiver(
    State(ui_api): State<UiApi>,
    Json(payload): Json<DeleteReceiver>,
) -> impl IntoResponse {
    if let Err(e) = ui_api.delete_receiver(payload.receiver).await {
        log::error!("Receiver deletion failed: {e}");
        return (
            StatusCode::BAD_REQUEST,
            Json(ReceiverDeleted {
                result: None,
                error: Some(e.to_string()),
            }),
        );
    }

    (
        StatusCode::OK,
        Json(ReceiverDeleted {
            result: Some("OK".to_owned()),
            error: None,
        }),
    )
}

async fn wb_config(State(ui_api): State<UiApi>) -> impl IntoResponse {
    let config = ui_api.wb_config_for_web().await;
    Json(config)
}

#[derive(Deserialize)]
struct CreateTransmitter {
    sdp: String,
}

#[derive(Deserialize)]
struct ReceiveStream {
    sdp: String,
}

#[derive(Deserialize)]
struct DeleteReceiver {
    receiver: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TransmitterCreated {
    session_id: Option<String>,
    error: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ReceivingStream {
    result: Option<String>,
    error: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ReceiverDeleted {
    result: Option<String>,
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
