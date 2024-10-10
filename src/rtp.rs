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

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Eq, Ord, Hash, Serialize, Deserialize)]
pub struct Channel {
    pub transceiver_id: usize,
    pub channel_nr: usize,
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
    mapping: HashMap<usize, (Channel, RxDescriptor)>,
    rev_mapping: HashMap<Channel, Vec<usize>>,
}

#[derive(Debug, Clone)]
pub struct InputMatrix {
    channels: usize,
    mapping: HashMap<Channel, usize>,
}

impl OutputMatrix {
    pub fn default(channels: usize) -> OutputMatrix {
        let mapping = HashMap::with_capacity(channels);
        let rev_mapping = HashMap::with_capacity(channels);
        OutputMatrix {
            channels,
            mapping,
            rev_mapping,
        }
    }

    pub fn get_outputs(&self, ch: &Channel) -> Option<&Vec<usize>> {
        self.rev_mapping.get(ch)
    }

    pub fn get_channel(&self, output: usize) -> Option<&(Channel, RxDescriptor)> {
        self.mapping.get(&output)
    }

    pub fn auto_route(
        &mut self,
        receiver: usize,
        channels: usize,
        desc: RxDescriptor,
    ) -> Option<Vec<usize>> {
        if self.channels - self.mapping.len() < channels {
            return None;
        }

        let mut ports = Vec::new();

        let mut channel = 0;
        for port in 0..self.channels {
            match self.mapping.entry(port) {
                Entry::Occupied(_) => continue,
                Entry::Vacant(e) => {
                    e.insert((Channel::new(receiver, channel), desc.clone()));
                    ports.push(port);
                    channel += 1;
                    if channel >= channels {
                        break;
                    }
                }
            }
        }

        self.update_rev_mapping();

        Some(ports.into())
    }

    pub fn auto_unroute(&mut self, receiver: usize) -> Vec<usize> {
        let mut ports = Vec::new();
        for port in 0..self.channels {
            match self.mapping.entry(port) {
                Entry::Occupied(e) => {
                    if e.get().0.transceiver_id == receiver {
                        e.remove();
                        ports.push(port);
                    }
                }
                Entry::Vacant(_) => continue,
            }
        }

        self.update_rev_mapping();

        ports.into()
    }

    fn update_rev_mapping(&mut self) {
        self.rev_mapping.clear();
        for (port, (ch, _)) in &self.mapping {
            match self.rev_mapping.entry(*ch) {
                Entry::Occupied(mut entry) => {
                    entry.get_mut().push(*port);
                }
                Entry::Vacant(entry) => {
                    entry.insert(vec![*port]);
                }
            }
        }
    }
}

impl InputMatrix {
    pub fn default(channels: usize) -> InputMatrix {
        let mapping = HashMap::new();
        InputMatrix { channels, mapping }
    }
}
