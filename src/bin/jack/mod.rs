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
    error::RtpResult,
    status::{Output, Status, StatusApi},
    utils::{
        init_buffer, open_shared_memory_buffers, AudioBuffer, MediaClockTimestamp, RemoteMediaClock,
    },
    AudioSystemConfig,
};
use jack::{
    contrib::ClosureProcessHandler, AudioIn, AudioOut, Client, ClientOptions, Control, Port,
    ProcessScope,
};
use rtp_rs::Seq;
use tokio_graceful_shutdown::SubsystemHandle;

const CLIENT_NAME: &str = "AES67 VirtualSoundCard";

struct State {
    _in_ports: Vec<Port<AudioIn>>,
    out_ports: Vec<Port<AudioOut>>,
    status: StatusApi,
    jack_media_clock: Option<MediaClockTimestamp>,
    clock: RemoteMediaClock,
    input_buffer: AudioBuffer,
    output_buffer: AudioBuffer,
    input_sequence_numbers: Box<[Option<Seq>]>,
    output_sequence_numbers: Box<[Option<Seq>]>,
    drift_counter: usize,
}

pub async fn run(
    subsys: &SubsystemHandle,
    inputs: usize,
    outputs: usize,
    status: StatusApi,
    port: u16,
    mem_conf: AudioSystemConfig,
) -> RtpResult<()> {
    // TODO evaluate client status
    let (client, _status) = Client::new(CLIENT_NAME, ClientOptions::default())?;

    let (input_buffer, output_buffer) = open_shared_memory_buffers(&mem_conf)?;

    // transmitters are mapped to JACK inputs
    let mut _in_ports = vec![];
    for i in 0..inputs {
        _in_ports.push(client.register_port(&format!("in{}", i + 1), AudioIn::default())?);
    }

    // receivers are mapped to JACK outputs
    let mut out_ports = vec![];
    for i in 0..outputs {
        out_ports.push(client.register_port(&format!("out{}", i + 1), AudioOut::default())?);
    }

    let jack_media_clock = None;

    let input_sequence_numbers = init_buffer(inputs, |_| None);
    let output_sequence_numbers = init_buffer(inputs, |_| None);

    let clock = RemoteMediaClock::connect(port, client.sample_rate()).await?;

    let process = ClosureProcessHandler::with_state(
        State {
            _in_ports,
            out_ports,
            status,
            jack_media_clock,
            clock: clock.clone(),
            input_buffer,
            output_buffer,
            input_sequence_numbers,
            output_sequence_numbers,
            drift_counter: 0,
        },
        process,
        |_, _, _| Control::Continue,
    );

    let active_client = client.activate_async((), process)?;

    connect_ports(active_client.as_client(), outputs);

    subsys.on_shutdown_requested().await;

    clock.close();

    if let Err(e) = active_client.deactivate() {
        log::error!("Error stopping JACK client: {e}");
    } else {
        log::info!("JACK client stopped successfully.");
    }

    Ok(())
}

fn process(state: &mut State, client: &Client, ps: &ProcessScope) -> Control {
    let media_clock = state.clock.media_time();
    let mut jack_media_clock = if let Some(it) = state.jack_media_clock {
        it
    } else {
        media_clock
    };
    let drift = jack_media_clock - media_clock;

    // TODO find out cause of jumps on port connect
    // TODO is JACK clock monotonic?

    // severely out of sync, this will cause an audible jump

    let skipped_clock = if drift.abs() >= client.buffer_size() as i64 {
        state.drift_counter += 1;
        let persisting = state.drift_counter > 10;
        if persisting {
            log::warn!(
                "JACK media clock is {drift} samples off, resetting it to system media clock"
            );
            state
                .status
                .try_publish(Status::Output(Output::ClockOutOfSync(drift)))
                .ok();
            jack_media_clock = media_clock;
        }
        persisting
    } else {
        state.drift_counter = 0;
        false
    };

    // if jack clock is slightly off, bring them back together again

    if !skipped_clock && drift < -1 {
        // JACK media clock is BEHIND
        log::debug!("JACK media clock is {} samples late", drift.abs());
        state
            .status
            .try_publish(Status::Output(Output::ClockAdjust(1)))
            .ok();
        jack_media_clock = jack_media_clock.next();
    }

    if !skipped_clock && drift > 1 {
        // JACK media clock is AHEAD
        log::debug!("JACK media clock is {} samples early", drift);
        state
            .status
            .try_publish(Status::Output(Output::ClockAdjust(-1)))
            .ok();
        jack_media_clock = jack_media_clock.previous();
    }

    state.jack_media_clock = Some(jack_media_clock + client.buffer_size());

    for (port_nr, port) in state.out_ports.iter_mut().enumerate() {
        let buffer = port.as_mut_slice(ps);
        let seq = state.output_sequence_numbers[port_nr];

        let (seq, underrun) = state
            .output_buffer
            .read(jack_media_clock, seq, port_nr, buffer);
        state.output_sequence_numbers[port_nr] = seq;

        if let Some(underrun) = underrun {
            let status = Status::Output(Output::BufferUnderrun(port_nr, underrun));
            state.status.publish_blocking(status).ok();
        }
    }

    // TODO read transmitters

    Control::Continue
}

fn connect_ports(client: &Client, channels: usize) {
    // TODO set up routing according to persisted config

    for i in 0..channels {
        let tx = format!("{CLIENT_NAME}:out{}", i + 1);
        let rx = format!("REAPER:in{}", i + 1);

        if let Err(e) = client.connect_ports_by_name(&tx, &rx) {
            log::warn!("Could not connect {tx} to {rx}: {e}");
        }
    }
}
