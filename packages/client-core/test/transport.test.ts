import { expect, test } from "bun:test";
import { createV4DataChannels } from "../src/transport";

test("creates canonical reliable and loss-tolerant v4 lanes", () => {
  const calls: Array<{ label: string; init?: RTCDataChannelInit }> = [];
  const peer = {
    createDataChannel(label: string, init?: RTCDataChannelInit): RTCDataChannel {
      calls.push({ label, init });
      return { label } as RTCDataChannel;
    },
  };

  const channels = createV4DataChannels(peer);
  expect(channels.control.label).toBe("nanoctl.control.v4");
  expect(channels.motion.label).toBe("nanoctl.motion.v4");
  expect(channels.videoDelta.label).toBe("nanoctl.video-delta.v4");
  expect(channels.videoReference.label).toBe("nanoctl.video-reference.v4");
  expect(calls).toEqual([
    { label: "nanoctl.control.v4", init: undefined },
    { label: "nanoctl.motion.v4", init: { ordered: false, maxRetransmits: 0 } },
    { label: "nanoctl.video-delta.v4", init: { ordered: false, maxPacketLifeTime: 150 } },
    { label: "nanoctl.video-reference.v4", init: { ordered: false } },
  ]);
});
