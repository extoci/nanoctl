import { describe, expect, test } from "bun:test";
import type { AccessUnit } from "@nanoctl/protocol";
import { FrameSequencer } from "../src/index";

function unit(streamSequence: number, keyframe = false): AccessUnit {
  return {
    streamSequence,
    keyframe,
    codec: "h264",
    captureTimeUs: streamSequence * 16_666,
    payload: Uint8Array.from([streamSequence & 0xff]),
  };
}

function sequences(outcome: { emitted: AccessUnit[] }): number[] {
  return outcome.emitted.map((accessUnit) => accessUnit.streamSequence);
}

describe("FrameSequencer", () => {
  test("accepts a keyframe then contiguous deltas", () => {
    const sequencer = new FrameSequencer();
    expect(sequences(sequencer.push(unit(10, true)))).toEqual([10]);
    expect(sequences(sequencer.push(unit(11)))).toEqual([11]);
    expect(sequences(sequencer.push(unit(12)))).toEqual([12]);
  });

  test("requests a keyframe once when deltas arrive before a reference", () => {
    const sequencer = new FrameSequencer();
    const first = sequencer.push(unit(3));
    expect(first.requestKeyframe).toBe(true);
    expect(first.unreferenceableDropped).toBe(1);
    const second = sequencer.push(unit(4));
    expect(second.requestKeyframe).toBe(false);
    expect(second.unreferenceableDropped).toBe(1);
  });

  test("holds a bounded reorder window and emits buffered frames in order", () => {
    const sequencer = new FrameSequencer(2);
    sequencer.push(unit(10, true));
    expect(sequencer.push(unit(12)).buffered).toBe(1);
    const outcome = sequencer.push(unit(11));
    expect(sequences(outcome)).toEqual([11, 12]);
    expect(outcome.buffered).toBe(0);
  });

  test("sorts multiple reordered future frames before draining", () => {
    const sequencer = new FrameSequencer(3);
    sequencer.push(unit(10, true));
    sequencer.push(unit(13));
    expect(sequencer.push(unit(12)).buffered).toBe(2);
    const outcome = sequencer.push(unit(11));
    expect(sequences(outcome)).toEqual([11, 12, 13]);
  });

  test("overflowing the reorder window breaks the reference exactly once", () => {
    const sequencer = new FrameSequencer(1);
    sequencer.push(unit(10, true));
    expect(sequencer.push(unit(12)).buffered).toBe(1);
    const overflow = sequencer.push(unit(13));
    expect(overflow.requestKeyframe).toBe(true);
    expect(overflow.unreferenceableDropped).toBe(2);
    expect(overflow.buffered).toBe(0);

    const whileWaiting = sequencer.push(unit(14));
    expect(whileWaiting.requestKeyframe).toBe(false);
    expect(whileWaiting.unreferenceableDropped).toBe(1);

    const recovery = sequencer.push(unit(20, true));
    expect(sequences(recovery)).toEqual([20]);
    expect(sequences(sequencer.push(unit(21)))).toEqual([21]);
  });

  test("drops duplicate and already-presented sequences", () => {
    const sequencer = new FrameSequencer();
    sequencer.push(unit(10, true));
    sequencer.push(unit(11));
    const duplicate = sequencer.push(unit(11));
    expect(duplicate.duplicatesDropped).toBe(1);
    expect(sequences(duplicate)).toEqual([]);
  });

  test("a late reference cannot rewind the stream, including during recovery", () => {
    const sequencer = new FrameSequencer();
    sequencer.push(unit(0xffffffff, true));
    sequencer.push(unit(1, true));
    expect(sequences(sequencer.push(unit(0, true)))).toEqual([]);
    expect(sequences(sequencer.push(unit(2)))).toEqual([2]);
    sequencer.requireKeyframe();
    expect(sequences(sequencer.push(unit(1, true)))).toEqual([]);
    expect(sequences(sequencer.push(unit(4, true)))).toEqual([4]);
  });

  test("keeps newer deltas that cross-lane reordering delivered before their keyframe", () => {
    const sequencer = new FrameSequencer();
    const first = sequencer.push(unit(1, true));
    expect(first.emitted.map((emitted) => emitted.streamSequence)).toEqual([1]);
    expect(sequencer.push(unit(4, false)).emitted).toEqual([]);
    expect(sequencer.push(unit(3, false)).emitted).toEqual([]);
    const recovered = sequencer.push(unit(2, true));
    expect(recovered.emitted.map((emitted) => emitted.streamSequence)).toEqual([2, 3, 4]);
  });

  test("handles u32 sequence wraparound", () => {
    const sequencer = new FrameSequencer(2);
    sequencer.push(unit(0xfffffffe, true));
    expect(sequences(sequencer.push(unit(0xffffffff)))).toEqual([0xffffffff]);
    expect(sequences(sequencer.push(unit(0)))).toEqual([0]);
    expect(sequences(sequencer.push(unit(1)))).toEqual([1]);
  });
});

test("the default reorder window accepts sixteen later frames before the missing one arrives", () => {
  const sequencer = new FrameSequencer();
  sequencer.push(unit(0, true));
  for (let sequence = 2; sequence <= 17; sequence += 1) {
    expect(sequencer.push(unit(sequence)).requestKeyframe).toBe(false);
  }
  expect(sequencer.push(unit(1)).emitted.map((frame) => frame.streamSequence)).toEqual(
    Array.from({ length: 17 }, (_, index) => index + 1),
  );
});
