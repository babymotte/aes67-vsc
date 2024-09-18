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

use clap::Parser;
use miette::Result;
use ptp::ptp;
use rtp_rx::rtp_rx;
use rtp_tx::rtp_tx;
use sap::sap;
use std::{io, time::Duration};
#[cfg(all(not(target_env = "msvc"), feature = "jemalloc"))]
use tikv_jemallocator::Jemalloc;
use tokio::sync::mpsc;
use tokio_graceful_shutdown::{SubsystemBuilder, Toplevel};
use tracing_log::AsTrace;
use tracing_subscriber::EnvFilter;
use vsc_ui::vsc_ui;

mod ptp;
mod rtp_rx;
mod rtp_tx;
mod sap;
mod vsc_ui;

#[cfg(all(not(target_env = "msvc"), feature = "jemalloc"))]
#[global_allocator]
static GLOBAL: Jemalloc = Jemalloc;

#[derive(Parser)]
#[command(author, version, about = "A virtual AES67 soundcard", long_about = None)]
struct Args {
    #[command(flatten)]
    verbose: clap_verbosity_flag::Verbosity,
    /// The port to run the web UI on
    port: Option<u16>,
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenv::dotenv().ok();

    let args: Args = Args::parse();

    tracing_subscriber::fmt()
        .with_writer(io::stderr)
        .with_env_filter(EnvFilter::from_default_env())
        .with_max_level(args.verbose.log_level_filter().as_trace())
        .init();

    let port = args.port.unwrap_or(8080);

    Toplevel::new(move |s| async move {
        let (tx_tx, tx_rx) = mpsc::channel(1);
        let (rx_tx, rx_rx) = mpsc::channel(1);
        let (ptp_tx, ptp_rx) = mpsc::channel(1);
        let (sap_tx, sap_rx) = mpsc::channel(1);
        s.start(SubsystemBuilder::new("rtp-tx", |s| rtp_tx(s, tx_rx)));
        s.start(SubsystemBuilder::new("rtp-rx", |s| rtp_rx(s, rx_rx)));
        s.start(SubsystemBuilder::new("ptp", |s| ptp(s, ptp_rx)));
        s.start(SubsystemBuilder::new("sap", |s| sap(s, sap_rx)));
        s.start(SubsystemBuilder::new("vsc-ui", move |s| {
            vsc_ui(s, tx_tx, rx_tx, ptp_tx, sap_tx, port)
        }));
    })
    .catch_signals()
    .handle_shutdown_requests(Duration::from_millis(1000))
    .await?;

    Ok(())
}
