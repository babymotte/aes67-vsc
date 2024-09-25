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

use std::future::Future;
use tokio::{
    select,
    sync::{mpsc, oneshot},
};
use tokio_util::sync::CancellationToken;

pub trait Actor {
    type Message;
    type Error;

    async fn recv_message(&mut self) -> Option<Self::Message>;
    async fn process_message(&mut self, msg: Self::Message) -> bool;

    async fn run(
        &mut self,
        name: String,
        cancel_token: CancellationToken,
    ) -> Result<(), Self::Error> {
        loop {
            select! {
                _ = cancel_token.cancelled() => {
                    log::info!("Shutdown requested, stopping actor {name} …");
                    break;
                },
                recv = self.recv_message() => match recv {
                    Some(msg) => if !self.process_message(msg).await {
                        break;
                    },
                    None => break,
                }
            }
        }

        Ok(())
    }
}

pub trait ActorApi: Clone {
    type Message;
    type Error;

    fn message_tx(&self) -> &mpsc::Sender<Self::Message>;

    async fn send_message<T>(
        &self,
        function: impl FnOnce(oneshot::Sender<Result<T, Self::Error>>) -> Self::Message,
    ) -> Result<T, Self::Error>
    where
        Self::Error: From<mpsc::error::SendError<Self::Message>> + From<oneshot::error::RecvError>,
    {
        let (res_tx, res_rx) = oneshot::channel();
        let function = function(res_tx);
        self.message_tx()
            .send(function)
            .await
            .map_err(|e| Self::Error::from(e))?;
        res_rx.await.map_err(|e| Self::Error::from(e))?
    }
}

pub async fn respond<T, E>(
    response: impl Future<Output = Result<T, E>>,
    tx: oneshot::Sender<Result<T, E>>,
) -> bool {
    tx.send(response.await).is_ok()
}
