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
};
use clap::Parser;
use miette::{IntoDiagnostic, Result};
use std::{io, time::Duration};
#[cfg(all(not(target_env = "msvc"), feature = "jemalloc"))]
use tikv_jemallocator::Jemalloc;
use tokio_graceful_shutdown::{SubsystemBuilder, SubsystemHandle, Toplevel};
use tracing_log::AsTrace;
use tracing_subscriber::EnvFilter;
use ui::ui;

#[cfg(all(not(target_env = "msvc"), feature = "jemalloc"))]
#[global_allocator]
static GLOBAL: Jemalloc = Jemalloc;

#[derive(Parser)]
#[command(author, version, about = "A virtual AES67 soundcard", long_about = None)]
struct Args {
    #[command(flatten)]
    verbose: clap_verbosity_flag::Verbosity,
    /// The port to run the web UI on
    #[arg(short, long)]
    port: Option<u16>,
    /// Number of input channels
    #[arg(short, long, default_value = "32")]
    inputs: usize,
    /// Number of output channels
    #[arg(short, long, default_value = "32")]
    outputs: usize,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    dotenv::dotenv().ok();

    let args: Args = Args::parse();

    tracing_subscriber::fmt()
        .with_writer(io::stderr)
        .with_env_filter(EnvFilter::from_default_env())
        .with_max_level(args.verbose.log_level_filter().as_trace())
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
    let port = args.port.unwrap_or(9090);

    let rtp_tx = RtpTxApi::new(&subsys).into_diagnostic()?;
    let rtp_rx = RtpRxApi::new(&subsys, args.inputs).into_diagnostic()?;
    let sap = SapApi::new(&subsys).into_diagnostic()?;
    let ptp = PtpApi::new(&subsys).into_diagnostic()?;

    ui(&subsys, rtp_tx, rtp_rx, ptp, sap, port)?;

    Ok(())
}
