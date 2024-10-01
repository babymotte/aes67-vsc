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

import {
  Divider,
  List,
  ListItemAvatar,
  ListItemButton,
  ListItemText,
  Stack,
  Typography,
} from "@mui/material";
import React from "react";
import { usePSubscribe, useSubscribe } from "worterbuch-react";
import VolumeUpIcon from "@mui/icons-material/VolumeUp";
import { useDeleteReceiver, useReceiveStream, WB_ROOT_KEY } from "../../api";
import { parse, SessionDescription } from "sdp-transform";
import VolumeOffIcon from "@mui/icons-material/VolumeOff";

export default function DiscoveryList() {
  const sdps = usePSubscribe<string>(WB_ROOT_KEY + "/discovery/?/?/?/sdp");
  const listItems: React.JSX.Element[] = [];

  sdps.forEach((v, k) => {
    listItems.push(<DiscoveryListItem key={k} sdp={v} />);
  });

  return <List sx={{ width: "100%" }}>{listItems}</List>;
}

function DiscoveryListItem({ sdp }: { sdp: string }) {
  const parsedSdp = parse(sdp);

  const sessionId = parsedSdp.origin?.sessionId;
  const sessionVersion = parsedSdp.origin?.sessionVersion;
  const receiver = useSubscribe<number>(
    `${WB_ROOT_KEY}/status/sessions/${sessionId}/${sessionVersion}/receiver`
  );

  const createReceiver = useReceiveStream(sdp);
  const deleteReceiver = useDeleteReceiver(receiver != null ? receiver : -1);

  return (
    <>
      <ListItemButton
        selected={receiver != null}
        onClick={receiver != null ? deleteReceiver : createReceiver}
      >
        <Stack direction="row" spacing={1} alignItems="center" width="100%">
          <Stack flexGrow={1}>
            <ListItemText
              primary={parsedSdp.name}
              secondary={
                <Typography
                  component="span"
                  variant="caption"
                  sx={{ color: "text.primary", display: "inline" }}
                >
                  {parsedSdp.description || channelsLabel(parsedSdp)}
                </Typography>
              }
            />
          </Stack>
          <Divider orientation="vertical" flexItem />
          <ListItemAvatar>
            <ReceiverAvatar receiver={receiver} />
          </ListItemAvatar>
        </Stack>
      </ListItemButton>
    </>
  );
}

function channelsLabel(sdp: SessionDescription) {
  const rtpmap = sdp.media[0]?.rtp[0];
  if (!rtpmap) {
    return "<unknown format>";
  }

  return `${rtpmap.encoding} channels`;
}

function ReceiverAvatar({ receiver }: { receiver: number | undefined }) {
  return (
    <Stack alignItems="center">
      {receiver != null ? (
        <VolumeUpIcon />
      ) : (
        <VolumeOffIcon sx={{ opacity: 0.2 }} />
      )}
      <Typography
        fontSize="0.5em"
        variant="caption"
        // sx={{ opacity: receiver != null ? 1.0 : 0.3 }}
      >
        Receiver: {receiver != null ? receiver : "-"}
      </Typography>
    </Stack>
  );
}
