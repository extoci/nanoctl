// Gamepad uplink wire cost for synthetic 60 Hz traces through the real
// GamepadForwarder and binary encoder. The comparison isolates shrinking the
// always-on application duplicate window now that SCTP performs bounded loss
// repair on the gamepad lane. Run: bun scripts/bench/gamepad-wire.ts
import { encodeBinaryInput, encodeGamepadHeartbeat } from "../../apps/viewer/src/lib/binary-input";
import { GamepadForwarder } from "../../apps/viewer/src/lib/gamepad";

// IPv4 + UDP + DTLS 1.2 AES-GCM record + SCTP common header + DATA chunk.
const OVERHEAD = 20 + 8 + 37 + 12 + 16;
const SECONDS = 60;
const FRAME_MS = 1000 / 60;

type State = { axes: number[]; pressed: number[] };
const traces: Record<string, (frame: number) => State> = {
  "idle pad (connected, untouched)": () => ({
    axes: [0, 0, 0, 0],
    pressed: [],
  }),
  "menu navigation (d-pad tap every 1s)": (frame) => ({
    axes: [0, 0, 0, 0],
    pressed: frame % 60 < 6 ? [13] : [],
  }),
  "racing (held throttle, steering bursts)": (frame) => ({
    axes: [frame % 180 < 30 ? Math.sin(frame / 5) : 0, 0, 0, 0],
    pressed: [7, ...(frame % 240 < 10 ? [0] : [])],
  }),
  "shooter (both sticks always moving)": (frame) => ({
    axes: [Math.sin(frame / 20), Math.cos(frame / 23), Math.sin(frame / 7), Math.cos(frame / 11)],
    pressed: frame % 30 < 4 ? [7] : [],
  }),
};

function pad({ axes, pressed }: State, index = 0) {
  return {
    axes,
    buttons: Array.from({ length: 17 }, (_, button) => ({
      pressed: pressed.includes(button),
      touched: false,
      value: pressed.includes(button) ? 1 : 0,
    })),
    connected: true,
    id: `Xbox Wireless Controller ${index}`,
    index,
    mapping: "standard" as const,
  };
}

const measure = (
  heartbeat: boolean,
  keepaliveMs: number,
  sparse: boolean,
  padCount: number,
  trace: (frame: number) => State,
  fullRefreshMs: number,
  repeatMs: number,
) => {
  let messages = 0;
  let bytes = 0;
  const forwarder = new GamepadForwarder(
    (message) => {
      messages++;
      bytes +=
        (message.type === "gamepad-heartbeat"
          ? encodeGamepadHeartbeat().byteLength
          : (encodeBinaryInput(message)?.byteLength ?? 0)) + OVERHEAD;
      return true;
    },
    { repeatMs, keepaliveMs, heartbeat, sparse, fullRefreshMs },
  );
  for (let frame = 0; frame < SECONDS * 60; frame++) {
    forwarder.sample(
      Array.from({ length: padCount }, (_, index) =>
        pad(index === 0 ? trace(frame) : { axes: [0, 0, 0, 0], pressed: [] }, index),
      ),
      frame * FRAME_MS,
    );
  }
  return { messages, bytes };
};

for (const [name, trace] of Object.entries(traces)) {
  const before = measure(true, 250, true, 4, trace, 2_000, 100);
  const after = measure(true, 250, true, 4, trace, 2_000, 30);
  const kbps = (value: number) => ((value * 8) / SECONDS / 1000).toFixed(2);
  const improvement = (1 - after.bytes / before.bytes) * 100;
  console.log(
    `${name}: ${(before.messages / SECONDS).toFixed(1)} -> ${(after.messages / SECONDS).toFixed(1)} msg/s, ` +
      `${kbps(before.bytes)} -> ${kbps(after.bytes)} kbps (-${improvement.toFixed(1)}%)`,
  );
  if (after.bytes > before.bytes) process.exitCode = 1;
}

// Active keyboard/pointer traffic within the heartbeat window already proves
// viewer-to-host liveness. Sharing it with the gamepad watchdog removes the
// parallel heartbeat datagrams without changing the neutralization timeout.
const measureActiveInputLiveness = (shared: boolean) => {
  let messages = 0;
  let bytes = 0;
  let inputLivenessAt = -Infinity;
  const forwarder = new GamepadForwarder(
    (message) => {
      messages++;
      bytes +=
        (message.type === "gamepad-heartbeat"
          ? encodeGamepadHeartbeat().byteLength
          : (encodeBinaryInput(message)?.byteLength ?? 0)) + OVERHEAD;
      return true;
    },
    { repeatMs: 30, keepaliveMs: 250, heartbeat: true, sparse: true, fullRefreshMs: 2_000 },
    () => (shared ? inputLivenessAt : -Infinity),
  );
  const idlePad = pad({ axes: [0, 0, 0, 0], pressed: [] });
  for (let frame = 0; frame < SECONDS * 60; frame++) {
    const now = frame * FRAME_MS;
    if (Math.floor(now / 200) !== Math.floor((now - FRAME_MS) / 200)) {
      inputLivenessAt = now;
      messages++;
      bytes += 3 + OVERHEAD; // Compact keyboard/pointer input datagram.
    }
    forwarder.sample([idlePad, idlePad, idlePad, idlePad], now);
  }
  return { messages, bytes };
};

{
  const before = measureActiveInputLiveness(false);
  const after = measureActiveInputLiveness(true);
  const kbps = (value: number) => ((value * 8) / SECONDS / 1000).toFixed(2);
  const improvement = (1 - after.bytes / before.bytes) * 100;
  console.log(
    `idle gamepad during active input: ${(before.messages / SECONDS).toFixed(1)} -> ${(after.messages / SECONDS).toFixed(1)} msg/s, ` +
      `${kbps(before.bytes)} -> ${kbps(after.bytes)} kbps (-${improvement.toFixed(1)}%)`,
  );
  if (improvement < 35) process.exitCode = 1;
}
