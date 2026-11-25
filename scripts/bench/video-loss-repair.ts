/**
 * Models loss-recovery wire cost for video deltas. SCTP fragments large data-channel
 * messages into MTU-sized packets; without retransmission one lost fragment breaks the
 * whole reference chain and forces a keyframe. Time-bounded retransmission resends only
 * the lost fragments and usually lands inside the viewer's four-frame reorder window.
 */
const FPS = 60;
const SECONDS = 30;
const FRAMES = FPS * SECONDS;
const FRAGMENT_PAYLOAD = 1_200;
const MAX_KEYFRAMES_PER_SECOND = 4;
const REPAIR_LIFETIME_MS = 80;

const fragments = (bytes: number) => Math.max(1, Math.ceil(bytes / FRAGMENT_PAYLOAD));
const brokenProbability = (frameBytes: number, loss: number, retransmitted: boolean) => {
  const fragmentFailure = retransmitted ? loss * loss : loss;
  return 1 - Math.pow(1 - fragmentFailure, fragments(frameBytes));
};

function measure(deltaBytes: number, keyframeBytes: number, loss: number, rttMs: number) {
  const usefulRepair = rttMs <= REPAIR_LIFETIME_MS;
  const beforeBreak = brokenProbability(deltaBytes, loss, false);
  const afterBreak = usefulRepair
    ? brokenProbability(deltaBytes, loss, true)
    : brokenProbability(deltaBytes, loss, false);
  const keyframeCap = (MAX_KEYFRAMES_PER_SECOND * SECONDS) / FRAMES;
  const beforeKeyframes = Math.min(beforeBreak, keyframeCap) * FRAMES;
  const afterKeyframes = Math.min(afterBreak, keyframeCap) * FRAMES;
  const base = deltaBytes * FRAMES;
  const retransmitted = usefulRepair ? base * loss : 0;
  const before = base + beforeKeyframes * keyframeBytes;
  const after = base + retransmitted + afterKeyframes * keyframeBytes;
  return { before, after, beforeKeyframes, afterKeyframes, retransmitted };
}

for (const [name, deltaBytes, keyframeBytes] of [
  ["small desktop deltas", 700, 50_000],
  ["continuous gameplay motion", 2_500, 100_000],
  ["worst-case noise", 20_000, 150_000],
] as const) {
  for (const loss of [0.0025, 0.01] as const) {
    const result = measure(deltaBytes, keyframeBytes, loss, 20);
    const saved = (1 - result.after / result.before) * 100;
    const kbps = (bytes: number) => ((bytes * 8) / SECONDS / 1_000).toFixed(1);
    console.log(
      `${name}, ${(loss * 100).toFixed(2)}% fragment loss, 20ms RTT: ` +
        `${kbps(result.before)} -> ${kbps(result.after)} kbps (${saved.toFixed(1)}% less), ` +
        `keyframes ${result.beforeKeyframes.toFixed(1)} -> ${result.afterKeyframes.toFixed(1)} per ${SECONDS}s`,
    );
    if (result.after >= result.before) process.exitCode = 1;
  }
}
