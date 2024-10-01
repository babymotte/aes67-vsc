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

mod actor;
mod ui;

use aes67_vsc::{
    ptp::PtpApi,
    rtp::{RtpRxApi, RtpTxApi},
    sap::SapApi,
    status::StatusApi,
};
use clap::Parser;
use miette::{IntoDiagnostic, Result};
use std::{io, net::IpAddr, time::Duration};
#[cfg(all(not(target_env = "msvc"), feature = "jemalloc"))]
use tikv_jemallocator::Jemalloc;
use tokio::{select, sync::oneshot};
use tokio_graceful_shutdown::{SubsystemBuilder, SubsystemHandle, Toplevel};
use tracing_log::AsTrace;
use tracing_subscriber::EnvFilter;
use ui::ui;
use worterbuch_client::{connect_with_default_config, topic};

#[cfg(all(not(target_env = "msvc"), feature = "jemalloc"))]
#[global_allocator]
static GLOBAL: Jemalloc = Jemalloc;

#[derive(Parser)]
#[command(author, version, about = "A virtual AES67 soundcard", long_about = None)]
struct Args {
    #[command(flatten)]
    verbose: clap_verbosity_flag::Verbosity,
    /// The port to run the web UI on
    #[arg(short, long, default_value = "9090")]
    port: u16,
    /// Number of input channels
    #[arg(short, long, default_value = "32")]
    inputs: usize,
    /// Number of output channels
    #[arg(short, long, default_value = "32")]
    outputs: usize,
    /// Max link offset it ms
    #[arg(short, long, default_value = "20")]
    link_offset: f32,
    /// The local IP address to bind to
    #[arg()]
    ip: Option<IpAddr>,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    dotenv::dotenv().ok();

    let args: Args = Args::parse();

    tracing_subscriber::fmt()
        .with_writer(io::stderr)
        .with_max_level(args.verbose.log_level_filter().as_trace())
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    Toplevel::new(move |s| async move {
        s.start(SubsystemBuilder::new("aes67-vsc", |s| run(args, s)));
    })
    .catch_signals()
    .handle_shutdown_requests(Duration::from_millis(1000))
    .await?;

    Ok(())
}

async fn run(args: Args, subsys: SubsystemHandle) -> Result<()> {
    let port = args.port;
    let link_offset = args.link_offset;

    let (wb_disco, mut wb_on_disco) = oneshot::channel();

    let on_disconnect = async move {
        log::info!("Worterbuch disconnected, requesting shutdown.");
        wb_disco.send(()).ok();
    };
    let (wb, wb_cfg) = connect_with_default_config(on_disconnect)
        .await
        .into_diagnostic()?;

    // TODO get from config?
    let wb_root_key = "aes67-vsc".to_owned();

    wb.set_client_name(&wb_root_key).await.into_diagnostic()?;
    wb.set_grave_goods(&[&topic!(wb_root_key, "status", "#")])
        .await
        .into_diagnostic()?;

    let status =
        StatusApi::new(&subsys, wb.clone(), wb_root_key.clone() + "/status").into_diagnostic()?;
    let rtp_tx = RtpTxApi::new(&subsys).into_diagnostic()?;
    let rtp_rx = RtpRxApi::new(&subsys, args.inputs, link_offset, status.clone(), args.ip)
        .into_diagnostic()?;
    let sap = SapApi::new(&subsys, wb.clone(), wb_root_key.clone()).into_diagnostic()?;
    let ptp = PtpApi::new(&subsys).into_diagnostic()?;

    ui(&subsys, rtp_tx, rtp_rx, ptp, sap, port, wb.clone(), wb_cfg).await?;

    select! {
        _ = subsys.on_shutdown_requested() => (),
        _ = &mut wb_on_disco => subsys.request_shutdown(),
    }

    Ok(())
}
