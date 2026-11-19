import { expect, test } from "bun:test";
import { videoRates } from "./video-rates";

const sample = { id: "video", bytes: 10_000, frames: 1, timestamp: 1_000 };
test("first sample reports unknown rates rather than zero", () => {
  expect(videoRates(sample, null)).toEqual({ fps: null, bitrateKbps: null });
  expect(videoRates(sample, null, 30).fps).toBe(30);
});
test("keeps sub-kbps traffic visible and derives FPS when the browser omits it", () => {
  expect(videoRates({ ...sample, bytes: 10_034, frames: 2, timestamp: 2_000 }, sample)).toEqual({
    fps: 1,
    bitrateKbps: 0.272,
  });
  expect(videoRates({ ...sample, timestamp: 2_000 }, sample)).toEqual({ fps: 0, bitrateKbps: 0 });
});
test("counter resets, stream changes and invalid intervals start a new baseline", () => {
  for (const current of [
    { ...sample, id: "new" },
    { ...sample, bytes: 1, timestamp: 2_000 },
    sample,
  ]) {
    expect(videoRates(current, sample)).toEqual({ fps: null, bitrateKbps: null });
  }
  expect(videoRates({ ...sample, frames: undefined, timestamp: 2_000 }, sample).fps).toBeNull();
});
