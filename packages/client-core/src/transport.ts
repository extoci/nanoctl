import {
  CONTROL_CHANNEL_LABEL,
  MOTION_CHANNEL_LABEL,
  VIDEO_DELTA_CHANNEL_LABEL,
  VIDEO_REFERENCE_CHANNEL_LABEL,
} from "@nanoctl/protocol";

export type DataChannelFactory = Pick<RTCPeerConnection, "createDataChannel">;

export interface V4DataChannels {
  control: RTCDataChannel;
  motion: RTCDataChannel;
  videoDelta: RTCDataChannel;
  videoReference: RTCDataChannel;
}

/**
 * Creates the four canonical v4 lanes. Discrete control and reference-recovery
 * bytes use WebRTC's reliable ordered defaults; high-frequency motion and delta
 * video are unordered with zero retransmissions so stale bytes are never
 * retransmitted ahead of newer state.
 */
export function createV4DataChannels(peer: DataChannelFactory): V4DataChannels {
  const lossTolerant: RTCDataChannelInit = {
    ordered: false,
    maxRetransmits: 0,
  };
  return {
    control: peer.createDataChannel(CONTROL_CHANNEL_LABEL),
    motion: peer.createDataChannel(MOTION_CHANNEL_LABEL, lossTolerant),
    videoDelta: peer.createDataChannel(VIDEO_DELTA_CHANNEL_LABEL, lossTolerant),
    videoReference: peer.createDataChannel(VIDEO_REFERENCE_CHANNEL_LABEL),
  };
}
