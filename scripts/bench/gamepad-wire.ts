// Gamepad uplink wire cost for synthetic 60 Hz traces: messages and bytes per
// second through the real GamepadForwarder and binary encoder, versus sending
// a snapshot on every poll. Run: bun scripts/bench/gamepad-wire.ts
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
    { repeatMs: 100, keepaliveMs, heartbeat, sparse },
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
  const before = measure(true, 75, false, 4, trace);
  const after = measure(true, 75, true, 4, trace);
  const kbps = (value: number) => ((value * 8) / SECONDS / 1000).toFixed(2);
  const improvement = (1 - after.bytes / before.bytes) * 100;
  console.log(
    `${name}: ${(before.messages / SECONDS).toFixed(1)} -> ${(after.messages / SECONDS).toFixed(1)} msg/s, ` +
      `${kbps(before.bytes)} -> ${kbps(after.bytes)} kbps (-${improvement.toFixed(1)}%)`,
  );
  if (after.bytes > before.bytes) process.exitCode = 1;
}
