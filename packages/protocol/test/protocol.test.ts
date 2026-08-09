import { describe, expect, test } from "bun:test";
import {
  PROTOCOL_VERSION,
  assertSignalEnvelope,
  clampNormalizedCoordinate,
  normalizeSignalEnvelope,
} from "../src";

describe("signal validation", () => {
  test("accepts a valid trickled ICE candidate", () => {
    const signal = {
      version: PROTOCOL_VERSION,
      sessionId: "session_1",
      controllerEpoch: "controller-1",
      hostEpoch: "host-1",
      generation: 0,
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
      controllerEpoch: "controller-1",
      hostEpoch: "",
      generation: 0,
      sequence: 0,
      sender: "controller",
      sentAt: Date.now(),
      payload: { type: "offer", sdp: "x".repeat(1_000_001) },
    };
    expect(() => assertSignalEnvelope(signal)).toThrow("invalid SDP");
  });

  test("requires explicit v2 peer epochs and ICE generation", () => {
    const controllerOffer = {
      version: PROTOCOL_VERSION,
      sessionId: "session_1",
      controllerEpoch: "controller-1",
      hostEpoch: "",
      generation: 0,
      sequence: 0,
      sender: "controller",
      sentAt: 1,
      payload: { type: "offer", sdp: "v=0" },
    };
    expect(() => assertSignalEnvelope({ ...controllerOffer, controllerEpoch: undefined })).toThrow(
      "invalid controller epoch",
    );
    expect(() => assertSignalEnvelope({ ...controllerOffer, generation: undefined })).toThrow(
      "invalid signal generation",
    );
    expect(() =>
      assertSignalEnvelope({
        ...controllerOffer,
        sender: "host",
        hostEpoch: "",
        payload: { type: "answer", sdp: "v=0" },
      }),
    ).toThrow("invalid host epoch for signal sender");
  });

  test("normalizes legacy v1 signals and rejects impossible ICE generations", () => {
    const legacy = normalizeSignalEnvelope({
      version: 1,
      sessionId: "session_1",
      sequence: 0,
      sender: "controller",
      sentAt: 1,
      payload: { type: "offer", sdp: "v=0" },
    });
    expect(legacy.version).toBe(PROTOCOL_VERSION);
    expect(legacy.generation).toBe(0);
    expect(() =>
      normalizeSignalEnvelope({
        ...legacy,
        version: 1,
        generation: 1_000_001,
      }),
    ).toThrow("signal generation is too large");
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
