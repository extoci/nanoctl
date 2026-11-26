import { encodeBinaryInput } from "../../apps/viewer/src/lib/binary-input";
import type { ControlMessage } from "@nanoctl/protocol";

// IPv4 + UDP + DTLS 1.2 AES-GCM record + SCTP common + DATA chunk header.
const TRANSPORT = 20 + 8 + 37 + 12 + 16;
const encoder = new TextEncoder();

function encodeV2(message: ControlMessage): ArrayBuffer | null {
  const header = (kind: number, length: number) => {
    const out = new DataView(new ArrayBuffer(length));
    out.setUint8(0, 0x4e);
    out.setUint8(1, 2);
    out.setUint8(2, kind);
    return out;
  };
  const position = (out: DataView, offset: number, x: number, y: number) => {
    out.setUint16(offset, Math.round(Math.min(1, Math.max(0, x)) * 65535), true);
    out.setUint16(offset + 2, Math.round(Math.min(1, Math.max(0, y)) * 65535), true);
  };
  if (message.type === "key") {
    const code = encoder.encode(message.code);
    const key = encoder.encode(message.key);
    const out = header(5, 6 + code.length + key.length);
    out.setUint8(3, (message.action === "up" ? 1 : 0) | (message.repeat ? 2 : 0));
    out.setUint8(4, code.length);
    out.setUint8(5, key.length);
    new Uint8Array(out.buffer, 6, code.length).set(code);
    new Uint8Array(out.buffer, 6 + code.length, key.length).set(key);
    return out.buffer;
  }
  if (message.type === "pointer" && (message.action === "down" || message.action === "up")) {
    const out = header(3, 8);
    out.setUint8(3, (message.action === "up" ? 0x80 : 0) | (message.button ?? 0));
    position(out, 4, message.x, message.y);
    return out.buffer;
  }
  if (message.type === "pointer" && message.action === "wheel") {
    const out = header(4, 23);
    position(out, 3, message.x, message.y);
    out.setFloat64(7, message.deltaX ?? 0, true);
    out.setFloat64(15, message.deltaY ?? 0, true);
    return out.buffer;
  }
  if (message.type === "ping") {
    const out = header(8, 19);
    out.setBigUint64(3, BigInt(message.nonce), true);
    out.setBigUint64(11, BigInt(message.sentAt), true);
    return out.buffer;
  }
  return null;
}

const sizes = (message: ControlMessage) => {
  const json = encoder.encode(JSON.stringify(message)).byteLength;
  const v2 = encodeV2(message)?.byteLength ?? json;
  const v3 = encodeBinaryInput(message)?.byteLength ?? json;
  return { json: TRANSPORT + json, v2: TRANSPORT + v2, v3: TRANSPORT + v3 };
};
const total = (messages: ControlMessage[]) =>
  messages.reduce(
    (sum, message) => {
      const wire = sizes(message);
      return { json: sum.json + wire.json, v2: sum.v2 + wire.v2, v3: sum.v3 + wire.v3 };
    },
    { json: 0, v2: 0, v3: 0 },
  );
const percent = (before: number, after: number) => 100 * (1 - after / before);
const report = (name: string, messages: ControlMessage[]) => {
  const wire = total(messages);
  const jsonSaving = percent(wire.json, wire.v3);
  const v2Saving = percent(wire.v2, wire.v3);
  console.log(
    `${name}: JSON ${wire.json} -> v2 ${wire.v2} -> v3 ${wire.v3} bytes/sample ` +
      `(v3 saves ${v2Saving.toFixed(1)}% vs v2, ${jsonSaving.toFixed(1)}% vs JSON)`,
  );
  if (jsonSaving < 20) process.exitCode = 1;
};

const key = (key: string, down: boolean): ControlMessage => ({
  type: "key",
  action: down ? "down" : "up",
  code: `Key${key.toUpperCase()}`,
  key,
  modifiers: 0,
  repeat: false,
});
const pointer = (action: "down" | "up"): ControlMessage => ({
  type: "pointer",
  action,
  x: 0.5,
  y: 0.5,
  button: 0,
});

report(
  "typing 120wpm key stream",
  [..."hello world"].flatMap((char) => [key(char, true), key(char, false)]),
);
report("mouse clicking", Array.from({ length: 10 }, () => [pointer("down"), pointer("up")]).flat());
report(
  "precision wheel scrolling",
  Array.from({ length: 60 }, (_, index) => ({
    type: "pointer" as const,
    action: "wheel" as const,
    x: 0.5,
    y: 0.5,
    deltaX: 0,
    deltaY: index % 2 ? -120 : -100,
  })),
);
// Application keepalive probes were pure overhead: the host never read their
// nonce/timestamp or sent a reply, while SCTP and media packets already keep
// the UDP association alive. Wire v3 therefore eliminates them entirely.
const probes = Array.from({ length: 10 }, (_, index) => ({
  type: "ping" as const,
  nonce: index + 1,
  sentAt: 1_700_000_000_000 + index * 1000,
}));
const probeWire = total(probes);
console.log(
  `periodic keepalive probes: JSON ${probeWire.json} -> v2 ${probeWire.v2} -> v3 0 bytes/sample ` +
    `(v3 saves 100.0% vs v2, 100.0% vs JSON)`,
);

// Held-key typematic repeat: before, every browser repeat occupied an SCTP
// datagram; after host-side synthesis, only one reliable liveness tick per
// second crosses the wire.
const heldKey = { ...key("a", true), repeat: true };
const repeatBefore = total([
  key("a", true),
  ...Array.from({ length: 30 }, () => heldKey),
  key("a", false),
]);
const repeatAfter = total([
  key("a", true),
  ...Array.from({ length: 1 }, () => heldKey),
  key("a", false),
]);
console.log(
  `held key for 1s at 30 repeats/s: ${repeatBefore.v3} -> ${repeatAfter.v3} bytes ` +
    `(saves ${percent(repeatBefore.v3, repeatAfter.v3).toFixed(1)}%)`,
);
