import { expect, test } from "bun:test";
import { attribute } from "./attribution";

function fixture() {
  return {
    source: { offsetMs: 1000, uncertaintyMs: 0.1 },
    raw: {
      paints: [0, 10],
      viewerKeys: [{ at: 1, code: "Space", repeat: false, trusted: true }],
      attribution: {
        clock: { offsetMs: 1000, uncertaintyMs: 0.1 },
        sends: [{ at: 1.1 }],
        frames: [
          {
            sequence: 1,
            input: 1,
            at: 30,
            receiveTime: 20,
            presentationTime: 25,
            expectedDisplayTime: 30,
            processingDuration: 0.001,
          },
        ],
      },
    },
  };
}
const source = {
  keys: [{ at: 2, code: "Space", repeat: false, trusted: true }],
  inputPaints: [{ input: 1, at: 10 }],
};
const pixelLog = "I 1000 1\nS 1032 1 1\nS 1036 1 1";
const agentLog = Object.entries({
  damage: 1012,
  capture_start: 1013,
  capture_end: 1014,
  encode_start: 1015,
  encode_end: 1018,
  send_start: 1018.1,
  send_end: 1018.2,
  input_receive: 1001.2,
  inject_start: 1001.3,
  inject_end: 1001.4,
})
  .map(([stage, at]) => `stage="${stage}" sequence=1 mono_ms=${at}`)
  .join("\n");

test("joins monotonic frame/input stages and counts each first pixel only once", () => {
  const report = attribute(fixture(), source, pixelLog, agentLog);
  const rows = new Map(report.rows.map((row) => [row.segment, row]));
  expect(report.matchedPixelFrames).toBe(1);
  expect(rows.get("draw → damage received")?.p50).toBe(2);
  expect(rows.get("capture end → encode start")?.p50).toBe(1);
  expect(rows.get("encode start → end")?.p50).toBe(3);
  expect(rows.get("expectedDisplayTime → first pixel")?.p50).toBe(2);
  expect(rows.get("source DOM → repaint")?.p50).toBe(8);
  expect(rows.get("XTest → source repaint")?.p50).toBe(10);
});

test("empty observations do not fabricate zero-duration segments", () => {
  const data = fixture();
  data.raw.attribution.frames = [];
  expect(attribute(data, source, "", "").rows).toEqual([]);
});

test("missing/invalid native stages stay missing and negative clock deltas remain visible", () => {
  const data = fixture();
  data.raw.attribution.frames[0].expectedDisplayTime = 40;
  const report = attribute(data, source, pixelLog, "invalid trace");
  expect(report.rows.find((row) => row.segment === "capture start → end")).toBeUndefined();
  expect(
    report.rows.find((row) => row.segment === "expectedDisplayTime → first pixel")?.negative,
  ).toBe(1);
});
