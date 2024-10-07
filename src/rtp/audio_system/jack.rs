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
    utils::{
        frames_per_link_offset_buffer, init_buffer, read_f32_sample, set_realtime_priority,
        MediaClockTimestamp, PlayoutBufferReader, PlayoutBufferWriter,
    },
};
use jack::{
    contrib::ClosureProcessHandler, AudioIn, AudioOut, Client, ClientOptions, Control, Frames,
    Port, ProcessScope,
};
use std::{borrow::BorrowMut, thread};
use tokio::sync::{
    mpsc::{self},
    oneshot,
};
use tokio_graceful_shutdown::SubsystemHandle;

const CLIENT_NAME: &str = "AES67 VirtualSoundCard";

pub(crate) struct JackAudioSystem {
    cancel: Option<oneshot::Sender<()>>,
    sample_rate: usize,
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
    active_inputs: Box<[Option<(usize, PlayoutBufferWriter)>]>,
    active_outputs: Box<[Option<(usize, PlayoutBufferReader)>]>,
    // TODO make this a queue instead of just a single sample
    pending_samples: Box<[Option<RtpSample<f32>>]>,
    link_offset: f32,
    jack_media_clock: Option<MediaClockTimestamp>,
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
        link_offset: f32,
    ) -> RtpResult<Self> {
        let active_inputs = init_buffer(transmitters, |_| None);
        let active_outputs = init_buffer(receivers, |_| None);

        // TODO evaluate client status
        let (client, _status) = Client::new(CLIENT_NAME, ClientOptions::default())?;
        let sample_rate = client.sample_rate();
        let buffer_size = frames_per_link_offset_buffer(link_offset, sample_rate) as Frames / 2;
        // TODO don't set buffer size automatically, make that option available in the UI
        if let Err(e) = client.set_buffer_size(buffer_size) {
            log::error!("Could not set JACK buffer size: {e}");
        }

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
        let _transmitters = None;
        let receivers = None;
        let jack_media_clock = None;

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
                link_offset,
                jack_media_clock,
            },
            process,
            init_buffers,
        );

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

        Ok(Self {
            cancel: Some(st),
            sample_rate,
        })
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

    fn sample_rate(&self) -> usize {
        self.sample_rate
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

fn process(state: &mut State, client: &Client, ps: &ProcessScope) -> Control {
    let media_clock = MediaClockTimestamp::now(client.sample_rate(), state.link_offset, 0);
    let mut jack_media_clock = if let Some(it) = state.jack_media_clock {
        it
    } else {
        media_clock
    };
    let drift = jack_media_clock - media_clock;
    // dbg!(drift);

    // TODO find out cause of jumps on port connect
    // TODO is JACK clock monotonic?

    // severely out of sync, this will cause an audible jump
    if drift.abs() >= client.buffer_size() as i64 {
        log::warn!("JACK media clock is {drift} samples off, resetting it to system media clock");
        jack_media_clock = media_clock;
    } else
    // if jack clock is slightly off, bring them back together again
    if drift < 0 {
        // JACK media clock is BEHIND
        log::warn!("JACK media clock is {} samples late", drift.abs());
        jack_media_clock = jack_media_clock.next();
    } else if drift > 0 {
        // JACK media clock is AHEAD
        log::warn!("JACK media clock is {} samples early", drift);
        jack_media_clock = jack_media_clock.previous();
    }

    state.jack_media_clock = Some(jack_media_clock + client.buffer_size());

    if let Ok(msg) = state.msg_rx.try_recv() {
        match msg {
            Message::ActiveInputsChanged(it) => state.active_inputs = it,
            Message::ActiveOutputsChanged(it) => state.active_outputs = it,
        }
    }

    for (port_nr, port) in state.out_ports.iter_mut().enumerate() {
        let buffer = port.as_mut_slice(ps);

        if let Some((channel, ref mut playout_buffer)) = &mut state.active_outputs[port_nr] {
            let port_jack_media_clock = jack_media_clock + playout_buffer.desc.rtp_offset;

            for i in 0..buffer.len() {
                if let Some(value) =
                    playout_buffer.read(port_jack_media_clock + i as u32, *channel, read_f32_sample)
                {
                    buffer[i] = value;
                } else {
                    // TODO report buffer underrund
                    log::warn!(
                        "buffer underrun in receiver {} channel {} at timestamp {}",
                        playout_buffer.desc.id,
                        channel,
                        port_jack_media_clock
                    );
                }
            }
        } else {
            silence(buffer, 0.0, 0);
        }
    }

    // TODO read transmitters

    Control::Continue
}

// fn compensate_clock_drift(
//     state: &mut State,
//     client: &Client,
//     ps: &ProcessScope,
// ) -> MediaClockTimestamp {
//     let media_clock = MediaClockTimestamp::now(client.sample_rate(), state.link_offset);
//     let jack_clock = ps.last_frame_time() as u64;
//     let current_jack_clock_offset = jack_clock as i64 - media_clock.timestamp as i64;

//     if state.jack_clock_offset != 0 {
//         let drift = state.jack_clock_offset - current_jack_clock_offset;
//         dbg!(drift);
//         // let new_jack_clock_offset = if state.jack_clock_offset == 0 {
//         //     current_jack_clock_offset
//         // } else {
//         //     (state.jack_clock_offset * 99 + current_jack_clock_offset) / 100
//         // };
//         // let drift = new_jack_clock_offset - state.jack_clock_offset;
//         // if drift != 0 {
//         //     log::warn!("JACK clock drift: {drift}");
//         //     // TODO adjust media clock
//         // }
//         state.accumulated_jack_clock_offset += drift;
//         dbg!(state.accumulated_jack_clock_offset);
//         // media_clock.jump_to((jack_clock as i64 - new_jack_clock_offset) as u32)
//     }

//     state.jack_clock_offset = current_jack_clock_offset;
//     media_clock
// }

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
