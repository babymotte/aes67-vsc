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

use aes67_vsc::UiCommandTx;
use miette::Result;
use tokio::{select, sync::mpsc};
use tokio_graceful_shutdown::SubsystemHandle;

pub async fn rtp_tx(
    subsys: SubsystemHandle,
    _ui_commands: mpsc::Receiver<UiCommandTx>,
) -> Result<()> {
    loop {
        select! {
            _ = subsys.on_shutdown_requested() => break,
        }
    }

    subsys.request_shutdown();

    Ok(())
}
