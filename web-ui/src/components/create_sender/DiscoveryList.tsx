import {
  List,
  ListItemAvatar,
  ListItemButton,
  ListItemText,
  Typography,
} from "@mui/material";
import React from "react";
import { usePSubscribe, useSubscribe } from "worterbuch-react";
import parse from "sdp-parsing";
import VolumeUpIcon from "@mui/icons-material/VolumeUp";
import { useDeleteReceiver, useReceiveStream, WB_ROOT_KEY } from "../../api";

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

  const match = /([0-9]+) ([0-9]+) /.exec(parsedSdp.o);
  const sessionId = match ? match[1].trim() : "0";
  const sessionVersion = match ? match[2].trim() : "0";
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
          primary={parsedSdp.s}
          secondary={
            <Typography
              component="span"
              variant="caption"
              sx={{ color: "text.primary", display: "inline" }}
            >
              {parsedSdp.i || channelsLabel(parsedSdp)}
            </Typography>
          }
        />
      </ListItemButton>
    </>
  );
}

function channelsLabel(sdp: any) {
  const rtpmap = sdp.media[0]?.val_attrs?.rtpmap;
  if (!rtpmap) {
    return "<unknown format>";
  }

  const matches = /[0-9]+ .+\/[0-9]+\/([0-9]+)/.exec(rtpmap);
  if (!matches) {
    return "<unknown format>";
  }

  return `${matches[1].trim()} channels`;
}
