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

use tokio::{
    select,
    sync::{
        mpsc::{self, error::SendError},
        oneshot::{self, error::RecvError},
    },
};
use tokio_util::sync::CancellationToken;

pub trait Actor<C, E> {
    async fn recv_command(&mut self) -> Option<C>;
    async fn process_command(&mut self, command: Option<C>) -> Result<bool, E>;
    async fn run(&mut self, cancel_token: CancellationToken) -> Result<(), E> {
        loop {
            select! {
                _ = cancel_token.cancelled() => break,
                recv = self.recv_command() => if !self.process_command(recv).await? {
                    break;
                }
            }
        }

        Ok(())
    }
}

pub async fn send_function<T, F, E: Into<E2>, E2>(
    function: impl FnOnce(oneshot::Sender<Result<T, E2>>) -> F,
    channel: &mpsc::Sender<F>,
    send_error_mapper: impl Fn(SendError<F>) -> E,
    receive_error_mapper: impl Fn(RecvError) -> E,
) -> Result<T, E2> {
    let (res_tx, res_rx) = oneshot::channel();
    let function = function(res_tx);
    let send_to_aes67_err = |e| send_error_mapper(e);
    channel
        .send(function)
        .await
        .map_err(|e| send_to_aes67_err(e).into())?;
    let res = res_rx.await.map_err(|e| receive_error_mapper(e).into())?;
    res
}

pub async fn respond<T, E>(response: T, tx: oneshot::Sender<T>) -> Result<bool, E> {
    Ok(tx.send(response).is_ok())
}
