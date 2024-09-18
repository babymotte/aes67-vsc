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

use aes67_vsc::UiCommandSAP;
use miette::{IntoDiagnostic, Result};
use sap_rs::{Sap, SessionAnnouncement};
use sdp::SessionDescription;
use std::io::Cursor;
use tokio::{select, sync::mpsc};
use tokio_graceful_shutdown::SubsystemHandle;

pub async fn sap(
    subsys: SubsystemHandle,
    mut ui_commands: mpsc::Receiver<UiCommandSAP>,
) -> Result<()> {
    let mut sap = Sap::new().await.into_diagnostic()?;

    let sdp = SessionDescription::unmarshal(&mut Cursor::new(
        "v=0
o=- 3456789 3456789 IN IP4 192.168.178.118
s=AES VSC
i=2 channels: Left, Right
c=IN IP4 239.69.202.125/32
t=0 0
a=keywds:Dante
a=recvonly
m=audio 5004 RTP/AVP 98
a=rtpmap:98 L24/48000/2
a=ptime:1
a=ts-refclk:ptp=IEEE1588-2008:00-1D-C1-FF-FE-0E-10-C4:0
a=mediaclk:direct=0
",
    ))
    .into_diagnostic()?;

    let announcement = SessionAnnouncement::new(sdp).into_diagnostic()?;

    loop {
        select! {
            _ = subsys.on_shutdown_requested() => break,
            _ = sap.announce_session(announcement) => break,
        }
    }

    sap.delete_session().await.into_diagnostic()?;

    subsys.request_shutdown();

    Ok(())
}
