import { expect, test } from "bun:test";
import { V4VideoMetrics, selectedIceLink } from "./v4-video-metrics";

test("reports actual decoded dimensions and timing, and tracks gaps across wraparound", () => {
  const metrics = new V4VideoMetrics();
  expect(metrics.snapshot()).toMatchObject({
    resolution: null,
    jitterMs: null,
    decodeMs: null,
    skippedFrames: null,
  });

  metrics.received(0xffffffff, 20_000, 100);
  metrics.delivered(0xffffffff);
  metrics.received(0, 40_000, 136);
  metrics.delivered(0);
  metrics.received(3, 100_000, 190);
  metrics.delivered(3);
  metrics.decoded(1280, 720, 2);
  metrics.decoded(960, 540, 4);

  expect(metrics.snapshot()).toMatchObject({
    resolution: "960×540",
    jitterMs: 1,
    decodeMs: 3,
    skippedFrames: 2,
    skippedPercent: 40,
  });
});

test("takes RTT from the selected ICE pair and distinguishes zero from unavailable", () => {
  const stats = new Map([
    ["transport", { type: "transport", selectedCandidatePairId: "selected" }],
    [
      "stale",
      { type: "candidate-pair", nominated: true, state: "succeeded", currentRoundTripTime: 9 },
    ],
    ["selected", { type: "candidate-pair", currentRoundTripTime: 0, remoteCandidateId: "relay" }],
    ["relay", { type: "remote-candidate", candidateType: "relay" }],
  ]);
  expect(selectedIceLink(stats as unknown as RTCStatsReport)).toEqual({ rttMs: 0, route: "relay" });
  expect(selectedIceLink(new Map() as unknown as RTCStatsReport)).toEqual({
    rttMs: null,
    route: "unknown",
  });
});
