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

mod jack;

use aes67_vsc::{status::StatusApi, utils::set_realtime_priority, AudioSystemConfig};
use clap::Parser;
use miette::{IntoDiagnostic, Result};
use std::{io, time::Duration};
#[cfg(all(not(target_env = "msvc"), feature = "jemalloc"))]
use tikv_jemallocator::Jemalloc;
use tokio::{select, sync::oneshot};
use tokio_graceful_shutdown::{SubsystemBuilder, SubsystemHandle, Toplevel};
use tracing_subscriber::EnvFilter;
use uuid::Uuid;
use worterbuch_client::{connect_with_default_config, topic};

#[cfg(all(not(target_env = "msvc"), feature = "jemalloc"))]
#[global_allocator]
static GLOBAL: Jemalloc = Jemalloc;

#[derive(Parser)]
#[command(author, version, about = "JACK audio system for AES67 VirtualSoundCard", long_about = None)]
struct Args {
    /// The number of inputs to create
    #[arg(short, long, default_value = "8")]
    inputs: usize,
    /// The number of outputs to create
    #[arg(short, long, default_value = "8")]
    outputs: usize,
    /// PTP server port
    #[arg(short, long, default_value = "9092")]
    port: u16,
    /// The network interface to use for PTP
    #[arg()]
    iface: Option<String>,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    dotenv::dotenv().ok();

    let args: Args = Args::parse();

    tracing_subscriber::fmt()
        .with_writer(io::stderr)
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    set_realtime_priority();

    let mem_conf: AudioSystemConfig = read_memory_config()?;

    Toplevel::new(move |s| async move {
        s.start(SubsystemBuilder::new("aes67-vsc-jack", move |s| {
            run(s, args.inputs, args.outputs, mem_conf, args.port)
        }));
    })
    .catch_signals()
    .handle_shutdown_requests(Duration::from_millis(1000))
    .await?;

    Ok(())
}

async fn run(
    subsys: SubsystemHandle,
    inputs: usize,
    outputs: usize,
    mem_conf: AudioSystemConfig,
    port: u16,
) -> Result<()> {
    // TODO don't crash when wortebruch disconnects, try to reconnect and resume publishing stats

    let (wb_disco, mut wb_on_disco) = oneshot::channel();

    let on_disconnect = async move {
        log::info!("Worterbuch disconnected, requesting shutdown.");
        wb_disco.send(()).ok();
    };
    let (wb, _) = connect_with_default_config(on_disconnect)
        .await
        .into_diagnostic()?;

    // TODO should the namespace be persisted / loaded from persistence on start?
    let hostname = hostname::get()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|_| Uuid::new_v4().to_string());
    let wb_root_key = "aes67-vsc";
    let wb_namespace_key = topic!(wb_root_key, hostname, "jack");

    wb.set_client_name(&topic!(wb_namespace_key))
        .await
        .into_diagnostic()?;

    wb.set_grave_goods(&[&topic!(wb_namespace_key, "#")])
        .await
        .into_diagnostic()?;

    let status = StatusApi::new(&subsys, wb.clone(), topic!(wb_namespace_key, "status"))
        .into_diagnostic()?;

    jack::run(&subsys, inputs, outputs, status, port, mem_conf)
        .await
        .into_diagnostic()?;

    select! {
        _ = subsys.on_shutdown_requested() => (),
        _ = &mut wb_on_disco => subsys.request_shutdown(),
    }

    Ok(())
}

fn read_memory_config() -> Result<AudioSystemConfig> {
    log::info!("Reading shared memory cnfig from stdin …");
    let mut buffer = String::new();
    io::stdin().read_line(&mut buffer).into_diagnostic()?;
    Ok(serde_json::from_str(&buffer).into_diagnostic()?)
}
