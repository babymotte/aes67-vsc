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
    discovery_cleanup::cleanup_discovery,
    rtp::{RtpRxApi, RtpTxApi},
    sap::SapApi,
    status::StatusApi,
    utils::{cretae_shared_memory_buffers, print_ifaces, set_realtime_priority},
};
use clap::Parser;
use miette::{miette, IntoDiagnostic, Result};
use pnet::datalink::{self, NetworkInterface};
use std::{io, time::Duration};
#[cfg(all(not(target_env = "msvc"), feature = "jemalloc"))]
use tikv_jemallocator::Jemalloc;
use tokio::{select, sync::oneshot};
use tokio_graceful_shutdown::{SubsystemBuilder, SubsystemHandle, Toplevel};
use tracing_subscriber::EnvFilter;
use ui::ui;
use uuid::Uuid;
use worterbuch_client::{connect_with_default_config, topic};

#[cfg(all(not(target_env = "msvc"), feature = "jemalloc"))]
#[global_allocator]
static GLOBAL: Jemalloc = Jemalloc;

#[derive(Parser)]
#[command(author, version, about = "A virtual AES67 soundcard", long_about = None)]
struct Args {
    /// The port to run the web UI on
    #[arg(short, long, default_value = "9090")]
    port: u16,
    /// Number of input channels
    #[arg(short, long, default_value = "32")]
    inputs: usize,
    /// Number of output channels
    #[arg(short, long, default_value = "32")]
    outputs: usize,
    /// Sample rate
    #[arg(short, long, default_value = "48000")]
    sample_rate: usize,
    /// Link offset it ms
    #[arg(short, long, default_value = "20")]
    link_offset: f32,
    /// The local IP address to bind to
    #[arg()]
    iface: Option<String>,
    /// Open the web UI on start
    #[arg(long)]
    ui: bool,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    dotenv::dotenv().ok();

    let args: Args = Args::parse();

    let iface_name = if let Some(it) = args.iface.as_deref() {
        it
    } else {
        print_ifaces();
        return Err(miette!("network interface not specified"));
    };

    tracing_subscriber::fmt()
        .with_writer(io::stderr)
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    set_realtime_priority();

    Toplevel::new(move |s| async move {
        s.start(SubsystemBuilder::new("aes67-vsc", |s| run(args, s)));
    })
    .catch_signals()
    .handle_shutdown_requests(Duration::from_millis(1000))
    .await?;

    Ok(())
}

async fn run(args: Args, subsys: SubsystemHandle) -> Result<()> {
    let iface = if let Some(it) = get_network_iface(args.iface.as_deref()) {
        it
    } else {
        return Err(miette!("network interface not found"));
    };

    let ip = if let Some(it) = iface.ips.iter().filter(|it| !it.ip().is_loopback()).next() {
        it.to_owned()
    } else {
        return Err(miette!("network interface has no IP"));
    };

    if iface.mac.is_none() {
        return Err(miette!("network interface has no MAC address"));
    }

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

    // TODO should the namespace be persisted / loaded from persistence on start?
    let hostname = hostname::get()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|_| Uuid::new_v4().to_string());
    let wb_root_key = "aes67-vsc";
    let wb_namespace_key = topic!(wb_root_key, hostname);

    wb.set_client_name(&wb_namespace_key)
        .await
        .into_diagnostic()?;

    wb.set_grave_goods(&[&topic!(wb_namespace_key, "#")])
        .await
        .into_diagnostic()?;

    wb.set(
        topic!(wb_namespace_key, "config", "audio", "linkOffsetMs"),
        args.link_offset,
    )
    .await
    .into_diagnostic()?;
    wb.set(
        topic!(wb_namespace_key, "config", "io", "inputs"),
        args.inputs,
    )
    .await
    .into_diagnostic()?;
    wb.set(
        topic!(wb_namespace_key, "config", "io", "outputs"),
        args.outputs,
    )
    .await
    .into_diagnostic()?;
    wb.set(
        topic!(wb_namespace_key, "config", "network", "ip"),
        ip.ip().to_string(),
    )
    .await
    .into_diagnostic()?;
    wb.set(
        topic!(wb_namespace_key, "config", "network", "port"),
        args.port,
    )
    .await
    .into_diagnostic()?;

    let (input_buffer, output_buffer) =
        cretae_shared_memory_buffers(args.inputs, args.outputs, args.sample_rate, link_offset)
            .into_diagnostic()?;

    // let clock = ptp(iface, ip.ip(), wb.clone(), wb_namespace_key.clone()).await;
    let status = StatusApi::new(&subsys, wb.clone(), topic!(wb_namespace_key, "status"))
        .into_diagnostic()?;
    let rtp_tx = RtpTxApi::new(&subsys).into_diagnostic()?;
    let rtp_rx =
        RtpRxApi::new(&subsys, status.clone(), ip.ip(), output_buffer).into_diagnostic()?;
    let sap = SapApi::new(&subsys, wb.clone(), wb_root_key.to_owned()).into_diagnostic()?;

    cleanup_discovery(&subsys, wb.clone(), wb_root_key.to_owned());

    ui(
        &subsys, rtp_tx, rtp_rx, sap, port, wb_cfg, args.ui, hostname,
    )
    .await?;

    select! {
        _ = subsys.on_shutdown_requested() => (),
        _ = &mut wb_on_disco => subsys.request_shutdown(),
    }

    Ok(())
}

fn get_network_iface(name: Option<&str>) -> Option<NetworkInterface> {
    match name {
        Some(name) => {
            for iface in datalink::interfaces() {
                if iface.name == name {
                    return Some(iface);
                }
            }
            None
        }
        None => {
            for iface in datalink::interfaces() {
                if iface.is_up()
                    && iface.is_running()
                    && !iface.is_loopback()
                    && !iface.ips.is_empty()
                    && iface.mac.is_some()
                {
                    return Some(iface);
                }
            }
            None
        }
    }
}
