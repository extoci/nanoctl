import { expect, test } from "bun:test";
import { encodeVideoChunk } from "@nanoctl/protocol";
import { VideoStreamProcessor } from "../src/index";

test("a missing frame requests recovery once and no dependent frame is emitted before a keyframe", () => {
  let seed = 0x7e4a324d;
  function next() {
    seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0;
    return seed;
  }

  for (let trial = 0; trial < 128; trial += 1) {
    const base = trial < 64 ? 0xffffffff - trial : next();
    const missing = 1 + (next() % 12);
    const future = 6 + (next() % 8);
    const processor = new VideoStreamProcessor();
    const emitted: number[] = [];
    let requests = 0;
    const feed = (index: number, keyframe = false) => {
      const outcome = processor.pushEncodedChunk(
        encodeVideoChunk({
          streamSequence: (base + index) >>> 0,
          part: 0,
          parts: 1,
          codec: "h264",
          keyframe,
          captureTimeUs: index * 16_000,
          payload: new Uint8Array([index]),
        }),
      );
      emitted.push(...outcome.emitted.map((unit) => unit.payload[0]!));
      requests += Number(outcome.requestKeyframe);
    };

    feed(0, true);
    for (let index = 1; index < missing; index += 1) feed(index);
    for (let index = missing + 1; index <= missing + future; index += 1) {
      feed(index);
      if (next() % 2) feed(index);
    }
    const intactPrefix = Array.from({ length: missing }, (_, index) => index);
    expect(emitted).toEqual(intactPrefix);
    expect(requests).toBe(1);

    feed(missing + future + 1, true);
    feed(missing + future + 2);
    expect(emitted).toEqual([...intactPrefix, missing + future + 1, missing + future + 2]);
    expect(requests).toBe(1);
  }
});
