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

use aes67_vsc::{UiCommandPTP, UiCommandRx, UiCommandSAP, UiCommandTx};
use miette::Result;
use pnet::{datalink, ipnetwork::IpNetwork};
use tokio::{select, sync::mpsc};
use tokio_graceful_shutdown::SubsystemHandle;

pub async fn vsc_ui(
    subsys: SubsystemHandle,
    _tx_commands: mpsc::Sender<UiCommandTx>,
    _rx_commands: mpsc::Sender<UiCommandRx>,
    _ptp_commands: mpsc::Sender<UiCommandPTP>,
    _sap_commands: mpsc::Sender<UiCommandSAP>,
    port: u16,
) -> Result<()> {
    for iface in datalink::interfaces() {
        for ip in iface.ips {
            if let IpNetwork::V4(ip) = ip {
                log::info!("WebUI: http://{}:{}", ip.ip(), port);
            }
        }
    }

    loop {
        select! {
            _ = subsys.on_shutdown_requested() => break,
        }
    }

    subsys.request_shutdown();

    Ok(())
}
