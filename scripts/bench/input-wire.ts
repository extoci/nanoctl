import { encodeBinaryInput } from "../../apps/viewer/src/lib/binary-input";
import type { ControlMessage } from "@nanoctl/protocol";

// IPv4 + UDP + DTLS 1.2 AES-GCM record + SCTP common + DATA chunk header.
const TRANSPORT = 20 + 8 + 37 + 12 + 16;
const bytes = (message: ControlMessage) => {
  const binary = encodeBinaryInput(message);
  return {
    json:
      TRANSPORT + new TextEncoder().encode(JSON.stringify(message)).byteLength,
    binary:
      TRANSPORT +
      (binary?.byteLength ??
        new TextEncoder().encode(JSON.stringify(message)).byteLength),
  };
};
const total = (messages: ControlMessage[]) =>
  messages.reduce(
    (sum, message) => {
      const wire = bytes(message);
      return { json: sum.json + wire.json, binary: sum.binary + wire.binary };
    },
    { json: 0, binary: 0 },
  );
const report = (name: string, messages: ControlMessage[]) => {
  const wire = total(messages);
  const saving = 100 * (1 - wire.binary / wire.json);
  console.log(
    `${name}: ${wire.json} -> ${wire.binary} bytes/sample (-${saving.toFixed(1)}%), ${messages.length} messages`,
  );
  if (saving < 20) process.exitCode = 1;
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
report(
  "mouse clicking",
  Array.from({ length: 10 }, () => [pointer("down"), pointer("up")]).flat(),
);
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
report(
  "periodic latency probes",
  Array.from({ length: 10 }, (_, index) => ({
    type: "ping" as const,
    nonce: index + 1,
    sentAt: 1_700_000_000_000 + index * 1000,
  })),
);
