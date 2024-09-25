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

import React from "react";
import FormGroup from "@mui/material/FormGroup";
import TextField from "@mui/material/TextField";
import Button from "@mui/material/Button";
import { Stack, Typography } from "@mui/material";
import { useReceiveStream } from "../../api";

export default function CreateReceiverForm() {
  const [sdp, setSdp] = React.useState<string | undefined>();

  const create = useReceiveStream(sdp);
  const submit = () =>
    create()
      .then((sid) => console.log(sid))
      .catch((e) => console.error("Receiver creation failed:", e.message));

  return (
    <FormGroup>
      <Stack spacing={2} padding={2}>
        <Typography>Create Receiver</Typography>
        <TextField multiline onChange={(e) => setSdp(e.target.value)} />
        <Button variant="contained" onClick={submit}>
          Create
        </Button>
      </Stack>
    </FormGroup>
  );
}
