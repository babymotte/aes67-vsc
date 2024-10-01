import {
  List,
  ListItemAvatar,
  ListItemButton,
  ListItemText,
  Typography,
} from "@mui/material";
import React from "react";
import { usePSubscribe, useSubscribe } from "worterbuch-react";
import VolumeUpIcon from "@mui/icons-material/VolumeUp";
import { useDeleteReceiver, useReceiveStream, WB_ROOT_KEY } from "../../api";
import { parse, SessionDescription } from "sdp-transform";

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
        <ListItemAvatar>
          {receiver != null ? <VolumeUpIcon /> : null}
        </ListItemAvatar>
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
