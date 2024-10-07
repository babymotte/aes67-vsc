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

pub(crate) use jack::JackAudioSystem;

use crate::utils::{PlayoutBufferReader, PlayoutBufferWriter};

pub(crate) trait AudioSystem {
    type SampleFormat;

    fn close(&mut self);

    fn sample_rate(&self) -> usize;

    async fn active_outputs_changed(
        &self,
        output_mapping: Box<[Option<(usize, PlayoutBufferReader)>]>,
    );

    async fn active_inputs_changed(
        &self,
        input_mapping: Box<[Option<(usize, PlayoutBufferWriter)>]>,
    );
}

pub(crate) enum OutputEvent {
    BufferUnderrun(usize),
}
