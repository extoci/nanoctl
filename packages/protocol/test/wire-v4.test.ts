import { describe, expect, test } from "bun:test";
import { fileURLToPath } from "node:url";
import {
  CONTROL_CHANNEL_LABEL,
  MAX_ACCESS_UNIT_BYTES,
  MAX_VIDEO_CHUNK_BYTES,
  MAX_VIDEO_PARTS,
  MOTION_CHANNEL_LABEL,
  ReferenceTracker,
  VIDEO_DELTA_CHANNEL_LABEL,
  VIDEO_REFERENCE_CHANNEL_LABEL,
  VideoReassembler,
  WireError,
  encodeInputMessage,
  encodeVideoControlMessage,
  encodeVideoChunk,
  parseInputMessage,
  parseVideoChunk,
  parseVideoControlMessage,
  type InputMessage,
  type VideoChunk,
} from "../src/v4";

type Vector = {
  name: string;
  lane: "input" | "video";
  expect: "valid" | "error";
  hex: string;
  decoded?: Record<string, unknown>;
  error?: string;
};

function decodeHex(hex: string): Uint8Array {
  const out = new Uint8Array(hex.length / 2);
  for (let index = 0; index < out.length; index += 1) {
    out[index] = Number.parseInt(hex.slice(index * 2, index * 2 + 2), 16);
  }
  return out;
}

function encodeHex(bytes: Uint8Array): string {
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

const fixturePath = fileURLToPath(
  new URL("../../../crates/nanoctl-core/test-vectors/wire-v4.json", import.meta.url),
);
const fixture = (await Bun.file(fixturePath).json()) as {
  vectors: Vector[];
};

describe("shared v4 wire golden vectors", () => {
  for (const vector of fixture.vectors) {
    test(vector.name, () => {
      const bytes = decodeHex(vector.hex);
      if (vector.expect === "error") {
        try {
          if (vector.lane === "input") parseInputMessage(bytes);
          else parseVideoChunk(bytes);
          throw new Error("expected parser to reject vector");
        } catch (error) {
          expect(error).toBeInstanceOf(WireError);
          expect((error as WireError).code).toBe(vector.error);
        }
        return;
      }

      if (vector.lane === "input") {
        const parsed = parseInputMessage(bytes);
        expect(encodeHex(encodeInputMessage(parsed))).toBe(vector.hex);
        const decoded = vector.decoded;
        expect(parsed.kind).toBe(decoded?.kind);
        expect(parsed.sequence).toBe(decoded?.sequence);
        if (parsed.kind === "pointerMove") {
          expect(parsed.point.x).toBe(decoded?.x);
          expect(parsed.point.y).toBe(decoded?.y);
        }
        if (parsed.kind === "key") {
          expect(parsed.down).toBe(decoded?.down);
          expect(parsed.browserRepeat).toBe(decoded?.browserRepeat);
          expect(parsed.identity.code).toBe(decoded?.code);
          expect(parsed.identity.key).toBe(decoded?.key);
        }
      } else {
        const parsed = parseVideoChunk(bytes);
        expect(encodeHex(encodeVideoChunk(parsed))).toBe(vector.hex);
        expect(parsed.streamSequence).toBe(vector.decoded?.streamSequence);
        expect(parsed.part).toBe(vector.decoded?.part);
        expect(parsed.parts).toBe(vector.decoded?.parts);
        expect(parsed.codec).toBe(vector.decoded?.codec);
        expect(parsed.keyframe).toBe(vector.decoded?.keyframe);
        expect(parsed.captureTimeUs).toBe(vector.decoded?.captureTimeUs);
      }
    });
  }
});

describe("v4 lane contract", () => {
  test("exposes canonical data-channel labels", () => {
    expect(CONTROL_CHANNEL_LABEL).toBe("nanoctl.control.v4");
    expect(MOTION_CHANNEL_LABEL).toBe("nanoctl.motion.v4");
    expect(VIDEO_DELTA_CHANNEL_LABEL).toBe("nanoctl.video-delta.v4");
    expect(VIDEO_REFERENCE_CHANNEL_LABEL).toBe("nanoctl.video-reference.v4");
  });
});

describe("v4 video control", () => {
  test("roundtrips keyframe requests and rejects payloads", () => {
    const message = { kind: "keyframeRequest" } as const;
    const encoded = encodeVideoControlMessage(message);
    expect(Array.from(encoded)).toEqual([4, 0x41]);
    expect(parseVideoControlMessage(encoded)).toEqual(message);
    expect(() => parseVideoControlMessage(new Uint8Array([4, 0x41, 0]))).toThrow(WireError);
  });
});

describe("v4 input wire", () => {
  test("roundtrips pointer wheel and gamepad snapshots", () => {
    const wheel: InputMessage = {
      kind: "pointerWheel",
      sequence: 44,
      point: { x: 1, y: 2 },
      deltaX: -1.5,
      deltaY: 3.25,
    };
    expect(parseInputMessage(encodeInputMessage(wheel))).toEqual(wheel);

    const snapshot: InputMessage = {
      kind: "gamepadSnapshot",
      sequence: 45,
      pads: [
        {
          slot: 1,
          kind: "dualsense",
          buttons: 0x803,
          leftTrigger: 1,
          rightTrigger: 255,
          leftX: -32768,
          leftY: 32767,
          rightX: -2,
          rightY: 2,
        },
      ],
    };
    expect(parseInputMessage(encodeInputMessage(snapshot))).toEqual(snapshot);
  });

  test("rejects malformed input without partial decoding", () => {
    expect(() => parseInputMessage(new Uint8Array())).toThrowError(
      expect.objectContaining({ code: "empty" }),
    );
    expect(() =>
      encodeInputMessage({ kind: "text", sequence: 1, text: "x".repeat(1025) }),
    ).toThrowError(expect.objectContaining({ code: "invalidLength" }));
  });
});

function chunk(
  streamSequence: number,
  part: number,
  parts: number,
  keyframe: boolean,
  payload: number[],
): VideoChunk {
  return {
    streamSequence,
    part,
    parts,
    keyframe,
    codec: "h264",
    captureTimeUs: 123,
    payload: Uint8Array.from(payload),
  };
}

describe("v4 video reassembly and reference validation", () => {
  test("reassembles out-of-order parts across sequences", () => {
    const reassembler = new VideoReassembler();
    expect(reassembler.push(chunk(7, 1, 2, true, [2])).complete).toBeUndefined();
    const replaced = reassembler.push(chunk(8, 0, 1, false, [3]));
    expect(replaced.droppedIncompleteSequence).toBeUndefined();
    expect(replaced.complete?.streamSequence).toBe(8);
    expect(Array.from(replaced.complete?.payload ?? [])).toEqual([3]);

    const kept = reassembler.push(chunk(7, 0, 2, true, [1]));
    expect(Array.from(kept.complete?.payload ?? [])).toEqual([1, 2]);

    const first = reassembler.push(chunk(9, 1, 2, true, [2]));
    expect(first.complete).toBeUndefined();
    const second = reassembler.push(chunk(9, 0, 2, true, [1]));
    expect(Array.from(second.complete?.payload ?? [])).toEqual([1, 2]);
    expect(second.complete?.keyframe).toBe(true);
  });

  test("reference tracker accepts ordered deltas only after a keyframe", () => {
    const tracker = new ReferenceTracker();
    expect(tracker.inspect(1, false)).toBe("requestKeyframe");
    expect(tracker.inspect(1, false)).toBe("dropAwaitingKeyframe");
    expect(tracker.inspect(10, true)).toBe("accept");
    expect(tracker.inspect(11, false)).toBe("accept");
    expect(tracker.inspect(13, false)).toBe("requestKeyframe");
    expect(tracker.inspect(12, false)).toBe("dropAwaitingKeyframe");
  });
});

test("interleaved fragments share one reassembly byte budget", () => {
  const reassembler = new VideoReassembler();
  const fullChunk = new Uint8Array(MAX_VIDEO_CHUNK_BYTES);
  const count = Math.floor(MAX_ACCESS_UNIT_BYTES / fullChunk.length);
  const fragment = (sequence: number, part: number): VideoChunk => ({
    streamSequence: sequence,
    part,
    parts: MAX_VIDEO_PARTS,
    keyframe: false,
    codec: "h264",
    captureTimeUs: sequence,
    payload: fullChunk,
  });
  for (let index = 0; index < count; index += 1) {
    const outcome = reassembler.push(fragment(index % 2, Math.floor(index / 2)));
    expect(outcome.complete).toBeUndefined();
  }
  expect(() => reassembler.push(fragment(count % 2, Math.floor(count / 2)))).toThrowError(
    expect.objectContaining({ code: "invalidValue" }),
  );
});
