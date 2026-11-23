import { expect, test } from "bun:test";
import { analyze, distribution } from "./metrics";

test("nearest rank distributions and empty observations", () => {
  expect(distribution([4, 1, 3, 2]).p50).toBe(2);
  expect(distribution([]).p99).toBeNull();
});
test("counts unique markers, missing responses, gaps and video latency", () => {
  const report = analyze(
    "S 0 0 0 0.1 0.2\nI 1000 1\nS 1010 1 1 0.1 0.2\nS 1014 1 1 0.1 0.2\nS 1130 2 1 0.1 0.2\nI 1200 2\nS 1300 -1 -1 0.1 0.2\nQ 10 100",
    [0, 1000, 1120],
    0,
  );
  expect(report.inputMs.p50).toBe(10);
  expect(report.missingInputs).toEqual([2]);
  expect(report.uniqueFrames).toBe(2);
  expect(report.stallsOver100Ms).toBe(1);
  expect(report.videoMs.mean).toBe(10);
  expect(report.detector.validSamples).toBe(4);
});
test("rejects absent capture and reports no valid markers without fabricating values", () => {
  expect(() => analyze("", [], 0)).toThrow("No detector samples");
  expect(analyze("S 0 -1 -1 0 0\nS 4 -1 -1 0 0", [], 0).inputMs.count).toBe(0);
});
