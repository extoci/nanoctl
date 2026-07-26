import { describe, expect, test } from "bun:test";
import { PROTOCOL_VERSION, assertSignalEnvelope, clampNormalizedCoordinate } from "../src";

describe("signal validation", () => {
  test("accepts a valid trickled ICE candidate", () => {
    const signal = {
      version: PROTOCOL_VERSION,
      sessionId: "session_1",
      sequence: 2,
      sender: "host",
      sentAt: Date.now(),
      payload: {
        type: "ice-candidate",
        candidate: "candidate:1 1 UDP 1 192.0.2.1 5000 typ host",
        sdpMid: "0",
        sdpMLineIndex: 0,
      },
    };
    expect(() => assertSignalEnvelope(signal)).not.toThrow();
  });

  test("rejects oversized SDP", () => {
    const signal = {
      version: PROTOCOL_VERSION,
      sessionId: "session_1",
      sequence: 0,
      sender: "controller",
      sentAt: Date.now(),
      payload: { type: "offer", sdp: "x".repeat(1_000_001) },
    };
    expect(() => assertSignalEnvelope(signal)).toThrow("invalid SDP");
  });
});

describe("coordinates", () => {
  test("clamps invalid and out-of-range coordinates", () => {
    expect(clampNormalizedCoordinate(-1)).toBe(0);
    expect(clampNormalizedCoordinate(0.4)).toBe(0.4);
    expect(clampNormalizedCoordinate(2)).toBe(1);
    expect(clampNormalizedCoordinate(Number.NaN)).toBe(0);
  });
});
