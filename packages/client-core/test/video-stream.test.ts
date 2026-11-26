import { describe, expect, test } from "bun:test";
import { WireError, encodeVideoChunk, type VideoChunk } from "@nanoctl/protocol";
import { VideoStreamProcessor } from "../src/index";

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
    captureTimeUs: 1000 + streamSequence,
    payload: Uint8Array.from(payload),
  };
}

function encoded(
  streamSequence: number,
  part: number,
  parts: number,
  keyframe: boolean,
  payload: number[],
): Uint8Array {
  return encodeVideoChunk(chunk(streamSequence, part, parts, keyframe, payload));
}

describe("VideoStreamProcessor", () => {
  test("parses, reassembles, and emits a complete keyframe", () => {
    const processor = new VideoStreamProcessor();
    const first = processor.pushEncodedChunk(encoded(10, 1, 2, true, [2]));
    expect(first.emitted).toEqual([]);
    const second = processor.pushEncodedChunk(encoded(10, 0, 2, true, [1]));
    expect(second.emitted).toHaveLength(1);
    expect(second.emitted[0]?.streamSequence).toBe(10);
    expect(Array.from(second.emitted[0]?.payload ?? [])).toEqual([1, 2]);
  });

  test("passes complete deltas through the bounded reference sequencer", () => {
    const processor = new VideoStreamProcessor(2);
    processor.pushChunk(chunk(10, 0, 1, true, [0]));
    expect(processor.pushChunk(chunk(12, 0, 1, false, [2])).emitted).toEqual([]);
    const outcome = processor.pushChunk(chunk(11, 0, 1, false, [1]));
    expect(outcome.emitted.map((unit) => unit.streamSequence)).toEqual([11, 12]);
  });

  test("surfaces eviction when starting a fourth incomplete access unit", () => {
    const processor = new VideoStreamProcessor();
    processor.pushChunk(chunk(7, 0, 2, true, [1]));
    processor.pushChunk(chunk(8, 0, 2, true, [2]));
    processor.pushChunk(chunk(9, 0, 2, true, [3]));
    const outcome = processor.pushChunk(chunk(10, 0, 2, true, [4]));
    expect(outcome.droppedIncompleteSequence).toBe(7);
    expect(processor.pushChunk(chunk(8, 1, 2, true, [5])).emitted[0]?.streamSequence).toBe(8);
  });

  test("requests keyframe for first delta and malformed bytes do not mutate state", () => {
    const processor = new VideoStreamProcessor();
    expect(processor.pushChunk(chunk(3, 0, 1, false, [1])).requestKeyframe).toBe(true);
    expect(() => processor.pushEncodedChunk(Uint8Array.from([4, 0x40]))).toThrowError(WireError);
    const recovery = processor.pushChunk(chunk(9, 0, 1, true, [1]));
    expect(recovery.emitted[0]?.streamSequence).toBe(9);
  });
});

test("a complete newer frame does not evict an earlier delta's partial fragments", () => {
  const processor = new VideoStreamProcessor();
  processor.pushEncodedChunk(encoded(10, 0, 1, true, [0]));
  processor.pushEncodedChunk(encoded(11, 0, 2, false, [1]));
  expect(processor.pushEncodedChunk(encoded(12, 0, 1, false, [3])).emitted).toEqual([]);
  const outcome = processor.pushEncodedChunk(encoded(11, 1, 2, false, [2]));
  expect(outcome.emitted.map((unit) => [...unit.payload])).toEqual([[1, 2], [3]]);
  expect(outcome.requestKeyframe).toBe(false);
});
