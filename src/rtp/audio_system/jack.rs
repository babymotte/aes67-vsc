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

use super::{AudioSystem, ReceiverBufferInitCallback, TransmitterBufferInitCallback};
use crate::error::RtpResult;
use jack::{
    contrib::ClosureProcessHandler, AudioIn, AudioOut, Client, ClientOptions, Control, Frames,
    Port, ProcessScope,
};
use std::thread;
use tokio::sync::{mpsc, oneshot};
use tokio_graceful_shutdown::SubsystemHandle;

const CLIENT_NAME: &str = "AES67 VirtualSoundCard";

pub(crate) struct JackAudioSystem {
    cancel: Option<oneshot::Sender<()>>,
}

struct State {
    _in_ports: Vec<Port<AudioIn>>,
    out_ports: Vec<Port<AudioOut>>,
    _transmitters: Option<Box<[mpsc::Sender<f32>]>>,
    _transmitter_init: TransmitterBufferInitCallback<f32>,
    receivers: Option<Box<[mpsc::Receiver<f32>]>>,
    receiver_init: ReceiverBufferInitCallback<f32>,
}

impl JackAudioSystem {
    pub(crate) fn new(
        _subsys: &SubsystemHandle,
        transmitters: usize,
        _transmitter_init: TransmitterBufferInitCallback<f32>,
        receivers: usize,
        receiver_init: ReceiverBufferInitCallback<f32>,
    ) -> RtpResult<Self> {
        // TODO evaluate client status
        let (client, _status) = Client::new(CLIENT_NAME, ClientOptions::default())?;

        // transmitters are mapped to JACK inputs
        let mut _in_ports = vec![];
        for i in 0..transmitters {
            _in_ports.push(client.register_port(&format!("in{}", i + 1), AudioIn::default())?);
        }

        // receivers are mapped to JACK outputs
        let mut out_ports = vec![];
        for i in 0..receivers {
            out_ports.push(client.register_port(&format!("out{}", i + 1), AudioOut::default())?);
        }

        let _transmitters = None;
        let receivers = None;

        let process = ClosureProcessHandler::with_state(
            State {
                _in_ports,
                out_ports,
                _transmitters,
                _transmitter_init,
                receivers,
                receiver_init,
            },
            process,
            init_buffers,
        );

        // 4. Activate the client

        let (st, op) = oneshot::channel();
        thread::spawn(|| {
            let active_client = client.activate_async((), process).unwrap();

            connect_ports(active_client.as_client());

            op.blocking_recv().ok();
            if let Err(e) = active_client.deactivate() {
                log::error!("Error stopping JACK client: {e}");
            } else {
                log::info!("JACK client stopped successfully.");
            }
        });

        Ok(Self { cancel: Some(st) })
    }
}

impl AudioSystem for JackAudioSystem {
    type SampleFormat = f32;

    fn close(&mut self) {
        log::info!("Closing JACK audio system …");
        if let Some(c) = self.cancel.take() {
            c.send(()).ok();
        }
    }
}

impl Drop for JackAudioSystem {
    fn drop(&mut self) {
        log::info!("JACK audio system was dropped.");
        self.close();
    }
}

fn init_buffers(state: &mut State, _: &Client, len: Frames) -> Control {
    log::info!(
        "Initializing JACK buffers on thread {:?}",
        thread::current()
    );

    let (tx, rx) = oneshot::channel();
    log::info!("Requesting receiver buffer …");
    if let Err(e) = state.receiver_init.blocking_send((len as usize, tx)) {
        log::error!("Could not initialize receiver buffer: {e}");
        return Control::Quit;
    }

    match rx.blocking_recv() {
        Ok(receivers) => state.receivers = Some(receivers),
        Err(e) => {
            log::error!("Could not initialize receiver buffer: {e}");
            return Control::Quit;
        }
    }

    log::info!("Receiver buffer initialized.");

    Control::Continue
}

fn process(state: &mut State, _client: &Client, ps: &ProcessScope) -> Control {
    if let Some(receivers) = state.receivers.as_mut() {
        for (i, port) in state.out_ports.iter_mut().enumerate() {
            let buffer = port.as_mut_slice(ps);
            let recv = &mut receivers[i];
            for sample in buffer {
                *sample = recv.try_recv().unwrap_or(0.0);
            }
        }
    } else {
        for port in &mut state.out_ports {
            silence(port.as_mut_slice(ps));
        }
    }

    // TODO read transmitters

    Control::Continue
}

fn connect_ports(client: &Client) {
    // TODO set up routing according to persisted config
    for i in 0..4 {
        let tx = format!("{CLIENT_NAME}:out{}", i + 1);
        let rx = format!("REAPER:in{}", i + 1);

        if let Err(e) = client.connect_ports_by_name(&tx, &rx) {
            log::warn!("Could not connect {tx} to {rx}: {e}");
        }
    }
}

fn silence(buffer: &mut [f32]) {
    for sample in buffer.iter_mut() {
        *sample = 0.0;
    }
}
