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

use aes67_vsc::{
    ptp::{
        ptp,
        statime_linux::{current_offset, PtpClock},
    },
    status::{Ptp, Status, StatusApi},
    utils::{print_ifaces, set_realtime_priority},
};
use clap::Parser;
use miette::{miette, IntoDiagnostic, Result};
use pnet::{
    datalink::{self, NetworkInterface},
    ipnetwork::IpNetwork,
};
use std::{io, net::SocketAddr, time::Duration};
#[cfg(all(not(target_env = "msvc"), feature = "jemalloc"))]
use tikv_jemallocator::Jemalloc;
use tokio::{
    io::AsyncWriteExt,
    net::{TcpSocket, TcpStream},
    select,
    sync::oneshot,
    time::{interval, timeout},
};
use tokio_graceful_shutdown::{SubsystemBuilder, SubsystemHandle, Toplevel};
use tracing_subscriber::EnvFilter;
use uuid::Uuid;
use worterbuch_client::{connect_with_default_config, topic};

#[cfg(all(not(target_env = "msvc"), feature = "jemalloc"))]
#[global_allocator]
static GLOBAL: Jemalloc = Jemalloc;

#[derive(Parser)]
#[command(author, version, about = "PTP system for AES67 VirtualSoundCard", long_about = None)]
struct Args {
    /// Port to run the time server on
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

    let iface_name = if let Some(it) = args.iface.as_deref() {
        it
    } else {
        print_ifaces();
        return Err(miette!("network interface not specified"));
    };

    let iface = if let Some(it) = get_network_iface(iface_name) {
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

    tracing_subscriber::fmt()
        .with_writer(io::stderr)
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    set_realtime_priority();

    Toplevel::new(move |s| async move {
        s.start(SubsystemBuilder::new("aes67-vsc-ptp", move |s| {
            run(s, iface, ip, args.port)
        }));
    })
    .catch_signals()
    .handle_shutdown_requests(Duration::from_millis(1000))
    .await?;

    Ok(())
}

async fn run(
    subsys: SubsystemHandle,
    iface: NetworkInterface,
    ip: IpNetwork,
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
    let wb_namespace_key = topic!(wb_root_key, hostname, "ptp");

    wb.set_client_name(&topic!(wb_namespace_key))
        .await
        .into_diagnostic()?;

    wb.set_grave_goods(&[&topic!(wb_namespace_key, "#")])
        .await
        .into_diagnostic()?;

    let clock = ptp(iface, ip.ip(), wb.clone(), wb_namespace_key.clone()).await;
    let status = StatusApi::new(&subsys, wb.clone(), topic!(wb_namespace_key, "status"))
        .into_diagnostic()?;

    let addr = format!("127.0.0.1:{port}").parse().unwrap();

    let socket = TcpSocket::new_v4().into_diagnostic()?;
    socket.set_reuseaddr(true).into_diagnostic()?;
    socket.bind(addr).into_diagnostic()?;

    let server = socket.listen(1024).into_diagnostic()?;

    let mut interval = interval(Duration::from_millis(100));

    let mut clients: Vec<(TcpStream, SocketAddr)> = Vec::new();

    let mut counter = 0;

    loop {
        select! {
            _ = subsys.on_shutdown_requested() => break,
            Ok(client) = server.accept() => {
                log::info!("Client connected: {}", client.1);
                clients.push(client);
            },
            _ = interval.tick() => update(&mut clients, &clock, &status, &mut counter).await?,
            _ = &mut wb_on_disco => subsys.request_shutdown(),
        }
    }

    Ok(())
}

async fn update(
    clients: &mut Vec<(TcpStream, SocketAddr)>,
    clock: &PtpClock,
    status: &StatusApi,
    counter: &mut usize,
) -> Result<()> {
    *counter += 1;

    let current_offset = current_offset(clock);

    let mut unresponsive = vec![];

    for (client, addr) in clients.iter_mut() {
        match timeout(
            Duration::from_millis(20),
            client.write(&current_offset.to_be_bytes()),
        )
        .await
        {
            Ok(Err(e)) => {
                log::error!("Error writing to client {addr}: {e}. Disconnecting …");
                unresponsive.push(*addr);
            }
            Ok(_) => (),
            Err(e) => {
                log::error!("Timeout while writing to client {addr}: {e}. Disconnecting …");
                unresponsive.push(*addr);
            }
        }
    }

    clients.retain(|(_, addr)| !unresponsive.contains(addr));

    if *counter % 10 == 0 {
        status
            .publish(Status::Ptp(Ptp::SystemOffset(current_offset)))
            .await
            .into_diagnostic()?;
    }

    Ok(())
}

fn get_network_iface(name: &str) -> Option<NetworkInterface> {
    for iface in datalink::interfaces() {
        if iface.name == name {
            return Some(iface);
        }
    }
    None
}
