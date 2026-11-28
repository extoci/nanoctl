import { expect, test } from "bun:test";
import { markerBits, readMarker, readMarkerRow, summarize } from "../tests/browser/latency-marker";

test("marker preserves unsigned sequence and input through the checksum", () => {
  for (const sequence of [0, 1, 0x8000, 0xffff]) {
    expect(readMarker(markerBits(sequence, 12))).toEqual({ sequence, input: 12 });
  }
});

test("marker rejects blank frames and ambiguous or corrupt bits", () => {
  expect(readMarker(Array<number>(36).fill(0))).toBeUndefined();
  const bits = markerBits(1, 2);
  expect(readMarker(bits.with(18, -1))).toBeUndefined();
  expect(readMarker(bits.with(18, 1 - bits[18]))).toBeUndefined();
});

test("latency distribution uses nearest-rank percentiles", () => {
  const samples = Array.from({ length: 100 }, (_, i) => 100 - i);
  expect(summarize(samples)).toEqual({
    count: 100,
    mean: 50.5,
    p50: 50,
    p95: 95,
    p99: 99,
    max: 100,
  });
  expect(() => summarize([])).toThrow("No latency samples");
});

test("the color guards locate a marker even when the source window is offset", () => {
  const pixels = new Uint8ClampedArray(640 * 4);
  const colors = [
    [0, 255, 255, 255],
    ...markerBits(72, 3).map((bit) => [bit * 255, bit * 255, bit * 255, 255]),
    [255, 0, 255, 255],
  ];
  for (const [i, color] of colors.entries()) {
    for (let x = 31 + i * 13; x < 31 + (i + 1) * 13; x++) pixels.set(color, x * 4);
  }
  expect(readMarkerRow(pixels)).toEqual({ sequence: 72, input: 3 });
  pixels.fill(0, 31 * 4, (31 + 13) * 4);
  expect(readMarkerRow(pixels)).toBeUndefined();
});
