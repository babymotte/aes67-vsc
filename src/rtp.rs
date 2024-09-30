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

mod audio_system;
mod rx;
mod socket;
mod tx;

pub use rx::*;
pub use tx::*;

use serde::{Deserialize, Serialize};
use std::{collections::HashMap, hash::Hash};

#[derive(Debug, Clone, PartialEq, PartialOrd, Eq, Ord, Hash, Serialize, Deserialize)]
pub struct Channel {
    transceiver_id: usize,
    channel_nr: usize,
}

impl Channel {
    pub fn new(transceiver_id: usize, channel_nr: usize) -> Self {
        Self {
            transceiver_id,
            channel_nr,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Matrix {
    Input(HashMap<Channel, usize>, usize),
    Output(HashMap<usize, Channel>, usize),
}

impl Matrix {
    pub fn default_output(outputs: usize) -> Matrix {
        let mut map = HashMap::new();
        for i in 0..outputs {
            map.insert(i, Channel::new(i / 2, i % 2));
        }
        Matrix::Output(map, outputs)
    }

    pub fn default_input(inputs: usize) -> Matrix {
        let mut map = HashMap::new();
        for i in 0..inputs {
            map.insert(Channel::new(i / 2, i % 2), i);
        }
        Matrix::Input(map, inputs)
    }
}
