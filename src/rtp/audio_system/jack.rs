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

use super::{
    AudioSystem, Message, OutputEvent, ReceiverBufferInitCallback, RtpSample,
    TransmitterBufferInitCallback,
};
use crate::{
    error::RtpResult,
    rtp::RxDescriptor,
    utils::{init_buffer, set_realtime_priority, MediaClockTimestamp},
};
use jack::{
    contrib::ClosureProcessHandler, AudioIn, AudioOut, Client, ClientOptions, Control, Frames,
    Port, ProcessScope,
};
use std::thread;
use tokio::sync::{
    mpsc::{self, error::TryRecvError},
    oneshot,
};
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
    receivers: Option<Box<[mpsc::Receiver<RtpSample<f32>>]>>,
    receiver_init: ReceiverBufferInitCallback<f32>,
    status: mpsc::Sender<OutputEvent>,
    msg_rx: mpsc::Receiver<Message>,
    active_inputs: Box<[bool]>,
    active_outputs: Box<[Option<RxDescriptor>]>,
    pending_samples: Box<[Option<RtpSample<f32>>]>,
    media_clocks: Box<[Option<MediaClockTimestamp>]>,
}

impl JackAudioSystem {
    pub(crate) fn new(
        _subsys: &SubsystemHandle,
        transmitters: usize,
        _transmitter_init: TransmitterBufferInitCallback<f32>,
        receivers: usize,
        receiver_init: ReceiverBufferInitCallback<f32>,
        status: mpsc::Sender<OutputEvent>,
        msg_rx: mpsc::Receiver<Message>,
    ) -> RtpResult<Self> {
        let active_inputs = init_buffer(transmitters, |_| false);
        let active_outputs = init_buffer(receivers, |_| None);

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

        let pending_samples = init_buffer(receivers, |_| None);
        let timestamps = init_buffer(receivers, |_| None);
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
                status,
                active_inputs,
                active_outputs,
                msg_rx,
                pending_samples,
                media_clocks: timestamps,
            },
            process,
            init_buffers,
        );

        // 4. Activate the client

        let (st, op) = oneshot::channel();
        thread::spawn(|| {
            set_realtime_priority();

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
    if let Ok(msg) = state.msg_rx.try_recv() {
        match msg {
            Message::ActiveInputsChanged(it) => state.active_inputs = it,
            Message::ActiveOutputsChanged(it) => state.active_outputs = it,
        }
    }

    if let Some(receivers) = state.receivers.as_mut() {
        for (port_nr, port) in state.out_ports.iter_mut().enumerate() {
            let buffer = port.as_mut_slice(ps);

            if let Some(desc) = &state.active_outputs[port_nr] {
                let recv = &mut receivers[port_nr];
                let mut last = 0.0;

                let mut index = 0;

                let mut media_clock = if let Some(it) = state.media_clocks[port_nr].take() {
                    it
                } else {
                    // media clock has not been initialized, create a new one based on system time
                    MediaClockTimestamp::now(&desc)
                };

                while index < buffer.len() {
                    let next = if let Some(it) = state.pending_samples[port_nr].take() {
                        Ok(it)
                    } else {
                        recv.try_recv()
                    };

                    match next {
                        Ok(RtpSample(playout_time, value)) => {
                            // TODO check if all expected samples have arrived and report lost packet if not
                            if playout_time < media_clock {
                                let diff = playout_time - media_clock;
                                let destination = index as i64 + diff;
                                if destination > 0 {
                                    // sample belongs in this buffer, we just need to jump back a bit
                                    // TODO reduce log level
                                    log::warn!(
                                        "sample is late: {playout_time} < {media_clock} - backtracking {diff} samples"
                                    );
                                    // reset index to playout time
                                    index = destination as usize;
                                    // reset media clock to playout time
                                    media_clock = playout_time;
                                } else {
                                    // sample belonged to previous buffer, nothing we can do here
                                    // TODO report dropped packet
                                    log::warn!(
                                        "sample is late: {playout_time} < {media_clock} - dropping it"
                                    );
                                    continue;
                                }
                            } else if playout_time > media_clock {
                                let diff = (playout_time - media_clock) as usize;
                                let destination = index + diff;
                                if destination < buffer.len() {
                                    // sample belongs in this buffer, we just need to jump ahead a bit
                                    // TODO reduce log level
                                    log::warn!(
                                        "sample is early: {playout_time} > {media_clock} - skipping {diff} samples"
                                    );
                                    // fill buffer up to playout time with silence in case we don't backtrack
                                    for i in index..destination {
                                        buffer[i] = last;
                                    }
                                    // advance index to playout time
                                    index = destination;
                                    // advance media clock to playout time
                                    media_clock = playout_time;
                                } else {
                                    // sample belongs in the next buffer, we play out silence for now and try again later
                                    // TODO report buffer underrun
                                    log::warn!(
                                        "sample is early: {playout_time} > {media_clock} - waiting for next buffer"
                                    );
                                    state.pending_samples[port_nr] =
                                        Some(RtpSample(playout_time, value));
                                    silence(buffer, last, index);
                                    break;
                                }
                            }

                            buffer[index] = value;
                            last = value;
                            index += 1;
                            media_clock = media_clock.next();
                            state.media_clocks[port_nr] = Some(media_clock);
                            if index == buffer.len() {
                                break;
                            }
                        }
                        Err(e) => match e {
                            TryRecvError::Empty => {
                                state
                                    .status
                                    .try_send(OutputEvent::BufferUnderrun(port_nr))
                                    .ok();
                                silence(buffer, last, index);
                                break;
                            }
                            TryRecvError::Disconnected => {
                                silence(buffer, last, index);
                                break;
                            }
                        },
                    };
                }
            } else {
                silence(buffer, 0.0, 0);
            }
        }
    } else {
        for port in &mut state.out_ports {
            silence(port.as_mut_slice(ps), 0.0, 0);
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

fn silence(buffer: &mut [f32], value: f32, start: usize) {
    for sample in buffer[start..].iter_mut() {
        *sample = value;
    }
}
