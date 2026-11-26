import {
  CONTROL_CHANNEL_LABEL,
  MOTION_CHANNEL_LABEL,
  VIDEO_DELTA_CHANNEL_LABEL,
  VIDEO_REFERENCE_CHANNEL_LABEL,
} from "@nanoctl/protocol";

export type DataChannelFactory = Pick<RTCPeerConnection, "createDataChannel"> &
  Partial<Pick<RTCPeerConnection, "getStats">>;

const videoStats = new WeakMap<RTCDataChannel, () => Promise<RTCStatsReport>>();

export function readV4IceStats(videoDelta: RTCDataChannel): Promise<RTCStatsReport | undefined> {
  return videoStats.get(videoDelta)?.() ?? Promise.resolve(undefined);
}

export interface V4DataChannels {
  control: RTCDataChannel;
  motion: RTCDataChannel;
  videoDelta: RTCDataChannel;
  videoReference: RTCDataChannel;
}

/**
 * Creates the four canonical v4 lanes. Discrete control and reference-recovery
 * bytes use WebRTC's reliable ordered defaults. Motion can be superseded by a
 * newer update. Video deltas can repair transport loss within a short deadline
 * because losing one delta also makes later deltas unusable.
 */
export function createV4DataChannels(peer: DataChannelFactory): V4DataChannels {
  const lossTolerant: RTCDataChannelInit = {
    ordered: false,
    maxRetransmits: 0,
  };
  const lanes = {
    control: peer.createDataChannel(CONTROL_CHANNEL_LABEL),
    motion: peer.createDataChannel(MOTION_CHANNEL_LABEL, lossTolerant),
    videoDelta: peer.createDataChannel(VIDEO_DELTA_CHANNEL_LABEL, {
      ordered: false,
      maxPacketLifeTime: 150,
    }),
    videoReference: peer.createDataChannel(VIDEO_REFERENCE_CHANNEL_LABEL),
  };
  if (peer.getStats) {
    videoStats.set(lanes.videoDelta, peer.getStats.bind(peer));
  }
  return lanes;
}
