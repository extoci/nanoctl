import { describe, expect, test } from "bun:test";
import { encodeVideoChunk, type AccessUnit, type VideoChunk } from "@nanoctl/protocol";
import { V4VideoReceiver } from "../src/index";

function chunk(
  streamSequence: number,
  keyframe: boolean,
  payload: number[],
): VideoChunk {
  return {
    streamSequence,
    part: 0,
    parts: 1,
    keyframe,
    codec: "h264",
    captureTimeUs: 1000 + streamSequence,
    payload: Uint8Array.from(payload),
  };
}

function encoded(streamSequence: number, keyframe: boolean, payload: number[]): Uint8Array {
  return encodeVideoChunk(chunk(streamSequence, keyframe, payload));
}

function fakeChannel(): RTCDataChannel {
  return { binaryType: "blob", onmessage: null } as unknown as RTCDataChannel;
}

function receiver() {
  const videoDelta = fakeChannel();
  const videoReference = fakeChannel();
  const emitted: AccessUnit[] = [];
  const requests: number[][] = [];
  const errors: unknown[] = [];
  const receiver = new V4VideoReceiver({
    videoDelta,
    videoReference,
    onAccessUnit: (unit) => emitted.push(unit),
    sendKeyframeRequest: (bytes) => requests.push(Array.from(bytes)),
    onError: (error) => errors.push(error),
  });
  return { receiver, videoDelta, videoReference, emitted, requests, errors };
}

describe("V4VideoReceiver", () => {
  test("shares one bounded reference chain across both video lanes", () => {
    const state = receiver();
    state.receiver.handle(encoded(1, true, [1]));
    state.receiver.handle(encoded(2, false, [2]));
    expect(state.emitted.map((unit) => unit.streamSequence)).toEqual([1, 2]);
    expect(state.videoDelta.binaryType).toBe("arraybuffer");
    expect(state.videoReference.binaryType).toBe("arraybuffer");
    expect(state.videoDelta.onmessage).not.toBeNull();
    expect(state.videoReference.onmessage).not.toBeNull();
  });

  test("requests a keyframe exactly once when reference recovery starts", () => {
    const state = receiver();
    state.receiver.handle(encoded(4, false, [4]));
    state.receiver.handle(encoded(5, false, [5]));
    expect(state.emitted).toEqual([]);
    expect(state.requests).toEqual([[4, 0x41]]);
    state.receiver.handle(encoded(6, true, [6]));
    expect(state.emitted.map((unit) => unit.streamSequence)).toEqual([6]);
  });

  test("reports malformed bytes without throwing from the lane callback", () => {
    const state = receiver();
    expect(() => state.receiver.handle(Uint8Array.from([4, 0x40]))).not.toThrow();
    expect(state.errors.length).toBe(1);
  });

  test("dispose detaches only its own handlers", () => {
    const state = receiver();
    state.receiver.dispose();
    expect(state.videoDelta.onmessage).toBeNull();
    expect(state.videoReference.onmessage).toBeNull();
  });
});
