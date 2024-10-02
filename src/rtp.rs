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

pub(crate) mod audio_system;
mod rx;
mod socket;
mod tx;

pub use rx::*;
pub use tx::*;

use serde::{Deserialize, Serialize};
use std::{
    collections::{hash_map::Entry, HashMap},
    hash::Hash,
};

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
pub struct OutputMatrix {
    channels: usize,
    mapping: HashMap<usize, Channel>,
}

#[derive(Debug, Clone)]
pub struct InputMatrix {
    channels: usize,
    mapping: HashMap<Channel, usize>,
}

impl OutputMatrix {
    pub fn default(channels: usize) -> OutputMatrix {
        let mapping = HashMap::new();
        // for i in 0..outputs {
        //     map.insert(i, Channel::new(i / 2, i % 2));
        // }
        OutputMatrix { channels, mapping }
    }

    pub fn auto_route(&mut self, receiver: usize, channels: usize) -> Option<Vec<usize>> {
        if self.channels - self.mapping.len() < channels {
            return None;
        }

        let mut ports = Vec::new();

        let mut channel = 0;
        for port in 0..self.channels {
            match self.mapping.entry(port) {
                Entry::Occupied(_) => continue,
                Entry::Vacant(e) => {
                    e.insert(Channel::new(receiver, channel));
                    ports.push(port);
                    channel += 1;
                    if channel >= channels {
                        break;
                    }
                }
            }
        }

        Some(ports.into())
    }

    pub fn auto_unroute(&mut self, receiver: usize) -> Vec<usize> {
        let mut ports = Vec::new();
        for port in 0..self.channels {
            match self.mapping.entry(port) {
                Entry::Occupied(e) => {
                    if e.get().transceiver_id == receiver {
                        e.remove();
                        ports.push(port);
                    }
                }
                Entry::Vacant(_) => continue,
            }
        }
        ports.into()
    }
}

impl InputMatrix {
    pub fn default(channels: usize) -> InputMatrix {
        let mapping = HashMap::new();
        // for i in 0..inputs {
        //     map.insert(Channel::new(i / 2, i % 2), i);
        // }
        InputMatrix { channels, mapping }
    }
}
