import { describe, expect, test } from "bun:test";
import {
  MAX_RELIABLE_CONTROL_BUFFER_BYTES,
  reliableControlBufferIsSaturated,
} from "./control-backpressure";

describe("reliable control backpressure", () => {
  test("allows traffic through the bounded queue threshold", () => {
    expect(reliableControlBufferIsSaturated(MAX_RELIABLE_CONTROL_BUFFER_BYTES)).toBe(false);
  });

  test("fails closed above the threshold or for invalid counters", () => {
    expect(reliableControlBufferIsSaturated(MAX_RELIABLE_CONTROL_BUFFER_BYTES + 1)).toBe(true);
    expect(reliableControlBufferIsSaturated(Number.NaN)).toBe(true);
  });
});
