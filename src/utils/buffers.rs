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

use tokio::sync::mpsc;

pub fn init_buffer<T>(size: usize, init: impl Fn(usize) -> T) -> Box<[T]> {
    (0..size).map(init).collect::<Vec<T>>().into()
}

pub struct DoubleBuffer<T> {
    pub tx: mpsc::Sender<Box<[T]>>,
    pub rx: mpsc::Receiver<Box<[T]>>,
}

impl<T: Default> DoubleBuffer<T> {
    pub async fn new(size: usize) -> (DoubleBuffer<T>, DoubleBuffer<T>) {
        let buffer_a = init_buffer(size, |_| T::default());
        let buffer_b = init_buffer(size, |_| T::default());

        let (producer_tx, consumer_rx) = mpsc::channel(1);
        let (consumer_tx, producer_rx) = mpsc::channel(2);

        producer_tx
            .send(buffer_a)
            .await
            .expect("receiver cannot be dropped at this point");
        consumer_tx
            .send(buffer_b)
            .await
            .expect("receiver cannot be dropped at this point");

        (
            DoubleBuffer {
                tx: producer_tx,
                rx: producer_rx,
            },
            DoubleBuffer {
                tx: consumer_tx,
                rx: consumer_rx,
            },
        )
    }
}
