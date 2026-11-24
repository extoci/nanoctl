import { describe, expect, test } from "bun:test";
import { containsIdr } from "./webcodecs-video";

const nal = (type: number, payload: number[] = [0x88, 0x84]) => [0, 0, 0, 1, type, ...payload];

describe("containsIdr", () => {
  test("detects IDR slices and SPS-led access units", () => {
    expect(containsIdr(new Uint8Array(nal(0x65)))).toBe(true);
    expect(containsIdr(new Uint8Array([...nal(0x67), ...nal(0x68), ...nal(0x65)]))).toBe(true);
    expect(containsIdr(new Uint8Array([...nal(0x09, [0xf0]), ...nal(0x06), ...nal(0x65)]))).toBe(
      true,
    );
    expect(containsIdr(new Uint8Array([0, 0, 1, 0x65, 0x88, 0x84]))).toBe(true);
  });

  test("stops at the first non-IDR slice", () => {
    expect(containsIdr(new Uint8Array(nal(0x41)))).toBe(false);
    expect(containsIdr(new Uint8Array([...nal(0x09, [0xf0]), ...nal(0x01)]))).toBe(false);
    // Slice payload bytes after a non-IDR slice are never interpreted.
    expect(containsIdr(new Uint8Array([...nal(0x41), 0, 0, 1, 0x65]))).toBe(false);
    expect(containsIdr(new Uint8Array([...nal(0x41), ...nal(0x41)]))).toBe(false);
  });

  test("ignores truncated or empty input", () => {
    expect(containsIdr(new Uint8Array([]))).toBe(false);
    expect(containsIdr(new Uint8Array([0, 0, 1, 0x65]))).toBe(false);
    expect(containsIdr(new Uint8Array([0, 0, 0, 1]))).toBe(false);
  });
});
