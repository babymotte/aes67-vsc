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

import axios from "axios";
import React from "react";
import { Config } from "worterbuch-react";

export function useWbConfig(): [Config | undefined, string | undefined] {
  const [config, setConfig] = React.useState<Config | undefined>();
  const [error, setError] = React.useState<string | undefined>();
  React.useEffect(() => {
    axios
      .get("/api/v1/wb/config")
      .then((res) => {
        console.log("Got WB config:", res.data);
        setConfig(res.data);
      })
      .catch((e) => setError(e.message));
  }, []);
  return [config, error];
}

export function useCreateTransmitter(
  sdp: string | undefined
): () => Promise<string> {
  return () =>
    new Promise((res, rej) => {
      if (sdp) {
        axios
          .post("/api/v1/create/transmitter", { sdp })
          .then((resp) => {
            const { sessionId, error } = resp.data;
            if (sessionId) {
              res(sessionId);
            } else if (error) {
              rej(new Error(error));
            }
          })
          .catch((e) => rej(new Error(e.message)));
      } else {
        rej(new Error("SDP is empty"));
      }
    });
}
