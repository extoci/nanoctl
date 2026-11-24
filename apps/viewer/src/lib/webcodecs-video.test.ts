import { afterEach, describe, expect, test } from "bun:test";
import { attachDataChannelVideo, containsIdr } from "./webcodecs-video";

const originalWindow = globalThis.window;
const originalMediaStream = globalThis.MediaStream;
const originalVideoDecoder = (globalThis as { VideoDecoder?: unknown }).VideoDecoder;
const originalEncodedVideoChunk = (globalThis as { EncodedVideoChunk?: unknown }).EncodedVideoChunk;
const originalMediaStreamTrackGenerator = (globalThis as { MediaStreamTrackGenerator?: unknown })
  .MediaStreamTrackGenerator;

function defineGlobal(name: string, value: unknown): void {
  Object.defineProperty(globalThis, name, {
    configurable: true,
    value,
  });
}

afterEach(() => {
  defineGlobal("window", originalWindow);
  defineGlobal("MediaStream", originalMediaStream);
  defineGlobal("VideoDecoder", originalVideoDecoder);
  defineGlobal("EncodedVideoChunk", originalEncodedVideoChunk);
  defineGlobal("MediaStreamTrackGenerator", originalMediaStreamTrackGenerator);
});

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

describe("data-channel decoder backpressure", () => {
  const wireMessage = (seq: number, key: boolean, payload: number[]): ArrayBuffer => {
    const bytes = new Uint8Array(17 + payload.length);
    bytes[0] = seq & 0xff;
    bytes[1] = (seq >> 8) & 0xff;
    bytes[2] = (seq >> 16) & 0xff;
    bytes[3] = (seq >> 24) & 0xff;
    bytes[4] = key ? 1 : 0;
    bytes[7] = 1;
    bytes.set(payload, 17);
    return bytes.buffer;
  };

  test("suppresses the broken delta tail and requests a recovery keyframe", () => {
    defineGlobal("window", {});
    defineGlobal(
      "MediaStream",
      class FakeMediaStream {
        constructor(public readonly tracks: unknown[]) {}
      },
    );
    class FakeEncodedVideoChunk {
      readonly type: string;
      constructor(init: { type: string }) {
        this.type = init.type;
      }
    }
    class FakeVideoDecoder {
      static instance: FakeVideoDecoder;
      decodeQueueSize = 0;
      configureCount = 0;
      resetCount = 0;
      readonly decodedTypes: string[] = [];
      constructor() {
        FakeVideoDecoder.instance = this;
      }
      configure() {
        this.configureCount += 1;
      }
      reset() {
        this.resetCount += 1;
        this.decodeQueueSize = 0;
      }
      decode(chunk: FakeEncodedVideoChunk) {
        this.decodedTypes.push(chunk.type);
      }
      close() {}
    }
    class FakeTrackGenerator {
      readonly track = { stop() {} };
      readonly writable = new WritableStream();
    }
    defineGlobal("EncodedVideoChunk", FakeEncodedVideoChunk);
    defineGlobal("VideoDecoder", FakeVideoDecoder);
    defineGlobal("MediaStreamTrackGenerator", FakeTrackGenerator);

    const sent: string[] = [];
    const channel = {
      send: (message: string) => sent.push(message),
      onmessage: null as ((event: MessageEvent) => void) | null,
    };
    const video = {
      ownerDocument: { createElement: () => ({}) },
      play: async () => {},
      srcObject: null,
    };
    const handle = attachDataChannelVideo(
      channel as unknown as RTCDataChannel,
      null,
      video as unknown as HTMLVideoElement,
    );
    const frame = (seq: number, key: boolean) => {
      channel.onmessage!({
        data: wireMessage(seq, key, nal(key ? 0x65 : 0x41)),
      } as MessageEvent);
    };

    frame(0, true);
    expect(FakeVideoDecoder.instance.decodedTypes).toEqual(["key"]);
    expect(sent).toEqual(["KF"]);

    FakeVideoDecoder.instance.decodeQueueSize = 2;
    frame(1, false);
    expect(sent).toEqual(["KF", "KF"]);
    expect(FakeVideoDecoder.instance.decodedTypes).toEqual(["key"]);

    FakeVideoDecoder.instance.decodeQueueSize = 0;
    frame(2, false);
    expect(FakeVideoDecoder.instance.decodedTypes).toEqual(["key"]);

    frame(3, true);
    expect(FakeVideoDecoder.instance.decodedTypes).toEqual(["key", "key"]);
    expect(FakeVideoDecoder.instance.resetCount).toBe(1);
    expect(FakeVideoDecoder.instance.configureCount).toBe(2);
    expect(handle.stats().dropped).toBe(2);
    handle.drop();
  });
});
