/**
 * Low-latency video decode path: snoops RTP payloads from the WebRTC encoded
 * transform stream, depacketizes H.264, decodes with WebCodecs and draws to a
 * canvas. This bypasses the <video> element's jitter buffer / render queue
 * (~20-35ms floor in Chrome) at the cost of doing depacketization ourselves.
 *
 * The track still flows through to the normal media pipeline (frames are
 * re-enqueued downstream), so stats and the <video> fallback keep working.
 */

export interface WebCodecsVideoHandle {
  canvas: HTMLCanvasElement;
  drop(): void;
  stats(): { decoded: number; dropped: number };
}

interface EncodedVideoFrameLike {
  type: string;
  timestamp: number;
  data: ArrayBuffer;
  getMetadata(): { rtpTimestamp?: number; synchronizationSource?: number };
}

interface EncodedStreams {
  readable: ReadableStream<EncodedVideoFrameLike>;
  writable: WritableStream<EncodedVideoFrameLike>;
}

export function supportsWebCodecsVideo(receiver: RTCRtpReceiver): boolean {
  return (
    typeof VideoDecoder === "function" &&
    typeof (receiver as RTCRtpReceiver & { createEncodedStreams?: unknown })
      .createEncodedStreams === "function"
  );
}

/**
 * Chrome only allows createEncodedStreams before the receiver has seen any
 * traffic, so this must be called on the recvonly transceiver's receiver
 * right after addTransceiver — before negotiation, not in ontrack.
 */
export function createVideoEncodedStreams(receiver: RTCRtpReceiver): EncodedStreams | null {
  try {
    return (
      (
        receiver as RTCRtpReceiver & { createEncodedStreams?: () => EncodedStreams }
      ).createEncodedStreams?.() ?? null
    );
  } catch {
    return null;
  }
}

function containsIdr(annexB: Uint8Array): boolean {
  for (let i = 0; i + 4 < annexB.length; i++) {
    if (annexB[i] === 0 && annexB[i + 1] === 0 && annexB[i + 2] === 1) {
      const type = annexB[i + 3]! & 0x1f;
      if (type === 5 || type === 7) return true;
    }
  }
  return false;
}

interface VideoSink {
  canvas: HTMLCanvasElement;
  feed(data: Uint8Array, isKey: boolean, timestampUs: number): void;
  teardown(): void;
  stats(): { decoded: number; dropped: number };
}

interface VideoTrackGeneratorLike {
  track: MediaStreamTrack;
  writable: WritableStream<VideoFrame>;
}

function createVideoSink(video: HTMLVideoElement): VideoSink {
  // Prefer VideoTrackGenerator: it presents decoded frames through the same
  // composited video layer as a media stream track, avoiding both the jitter
  // buffer and the costly software canvas composite. Fall back to a 2D canvas
  // where the generator does not exist (e.g. Firefox).
  const g = globalThis as {
    MediaStreamTrackGenerator?: new (init: { kind: string }) => VideoTrackGeneratorLike;
    VideoTrackGenerator?: new () => VideoTrackGeneratorLike;
  };
  let generator: VideoTrackGeneratorLike | null = null;
  try {
    generator = g.MediaStreamTrackGenerator
      ? new g.MediaStreamTrackGenerator({ kind: "video" })
      : g.VideoTrackGenerator
        ? new g.VideoTrackGenerator()
        : null;
  } catch {
    generator = null;
  }
  const writer = generator?.writable.getWriter() ?? null;
  if (generator) {
    // Older builds expose `.track`; current Chrome makes the generator itself
    // a MediaStreamTrack subclass.
    const track = generator.track ?? (generator as unknown as MediaStreamTrack);
    video.srcObject = new MediaStream([track]);
    void video.play().catch(() => {});
  }
  let canvas: HTMLCanvasElement | null = null;
  let ctx: CanvasRenderingContext2D | null = null;
  if (!generator) {
    canvas = document.createElement("canvas");
    canvas.className = "webcodecs-video";
    canvas.setAttribute("aria-hidden", "true");
    video.insertAdjacentElement("afterend", canvas);
    ctx = canvas.getContext("2d", { alpha: false });
  }

  let closed = false;
  let decoder: VideoDecoder | null = null;
  let decoded = 0;
  let dropped = 0;

  const ensureDecoder = (annexB: Uint8Array) => {
    if (decoder || typeof VideoDecoder !== "function") return;
    // Derive the codec string from the first SPS we see (avc1.PPCCLL).
    let codec = "avc1.640028";
    for (let i = 0; i + 7 < annexB.length; i++) {
      if (annexB[i] === 0 && annexB[i + 1] === 0 && annexB[i + 2] === 1) {
        const nal = annexB[i + 3]!;
        if ((nal & 0x1f) === 7 && i + 7 < annexB.length) {
          const hex = (b: number) => b.toString(16).padStart(2, "0");
          codec = `avc1.${hex(annexB[i + 4]!)}${hex(annexB[i + 5]!)}${hex(annexB[i + 6]!)}`;
          break;
        }
      }
    }
    const next = new VideoDecoder({
      output: (frame) => {
        if (writer) {
          decoded += 1;
          // Close once the generator has consumed the frame.
          void writer
            .write(frame)
            .then(() => frame.close())
            .catch(() => frame.close());
          return;
        }
        try {
          if (ctx && canvas) {
            if (canvas.width !== frame.displayWidth || canvas.height !== frame.displayHeight) {
              canvas.width = frame.displayWidth;
              canvas.height = frame.displayHeight;
            }
            ctx.drawImage(frame, 0, 0, canvas.width, canvas.height);
            decoded += 1;
          }
        } finally {
          frame.close();
        }
      },
      error: (error) => {
        console.warn("webcodecs video decoder error", error);
        teardown();
      },
    });
    try {
      next.configure({
        codec,
        optimizeForLatency: true,
        latencyMode: "realtime",
      } as VideoDecoderConfig & { latencyMode: "realtime" });
      decoder = next;
    } catch (error) {
      console.warn("webcodecs video configure failed", error);
      teardown();
    }
  };

  // Receiver-side encoded-transform frames are already depacketized AUs;
  // Chrome delivers H.264 in Annex-B. Keep an AVCC/packetized fallback just in
  // case the framing differs.
  let decoderConfigured = false;

  const toAnnexB = (data: Uint8Array): Uint8Array => {
    if (
      data.length >= 4 &&
      ((data[0] === 0 && data[1] === 0 && data[2] === 1) ||
        (data[0] === 0 && data[1] === 0 && data[2] === 0 && data[3] === 1))
    ) {
      return data;
    }
    // Try length-prefixed (AVCC) NAL units -> Annex-B.
    const out: number[] = [];
    let offset = 0;
    let ok = true;
    while (offset + 4 <= data.length) {
      const len =
        (data[offset]! << 24) |
        (data[offset + 1]! << 16) |
        (data[offset + 2]! << 8) |
        data[offset + 3]!;
      offset += 4;
      if (len <= 0 || offset + len > data.length) {
        ok = false;
        break;
      }
      out.push(0, 0, 0, 1);
      for (let i = 0; i < len; i++) out.push(data[offset + i]!);
      offset += len;
    }
    if (!ok || offset !== data.length) {
      // Unknown framing: wrap the whole payload as a single NAL.
      const single = new Uint8Array(data.length + 4);
      single.set([0, 0, 0, 1], 0);
      single.set(data, 4);
      return single;
    }
    return new Uint8Array(out);
  };

  const feed = (data: Uint8Array, isKey: boolean, timestampUs: number) => {
    const annexB = toAnnexB(data);
    if (!decoderConfigured) {
      if (!isKey && !containsIdr(annexB)) {
        dropped += 1;
        return;
      }
      ensureDecoder(annexB);
      if (!decoder) return;
      decoderConfigured = true;
    }
    if (!decoder) return;
    if (decoder.decodeQueueSize > 1 && !isKey) {
      dropped += 1;
      return;
    }
    try {
      decoder.decode(
        new EncodedVideoChunk({
          type: isKey ? "key" : "delta",
          timestamp: timestampUs,
          data: annexB,
        }),
      );
    } catch {
      decoderConfigured = false;
    }
  };

  const teardown = () => {
    if (closed) return;
    closed = true;
    try {
      decoder?.close();
    } catch {}
    if (generator) {
      void writer?.close().catch(() => {});
      (generator.track ?? (generator as unknown as MediaStreamTrack)).stop();
      if (video.srcObject instanceof MediaStream) video.srcObject = null;
    }
    canvas?.remove();
  };

  return {
    canvas: canvas ?? video.ownerDocument.createElement("canvas"),
    feed,
    teardown,
    stats: () => ({ decoded, dropped }),
  };
}

export function attachWebCodecsVideo(
  streams: EncodedStreams,
  video: HTMLVideoElement,
): WebCodecsVideoHandle {
  const sink = createVideoSink(video);
  const wrappedTransform = new TransformStream<EncodedVideoFrameLike, EncodedVideoFrameLike>({
    transform: (frame, controller) => {
      try {
        const rtpTimestamp = frame.getMetadata().rtpTimestamp ?? 0;
        const data = new Uint8Array(frame.data);
        const isKey = frame.type === "key" || containsIdr(data);
        sink.feed(data, isKey, Math.round(rtpTimestamp * (1e6 / 90_000)));
      } catch {
        // Tap must never break the normal pipeline.
      }
      controller.enqueue(frame);
    },
  });
  streams.readable
    .pipeThrough(wrappedTransform)
    .pipeTo(streams.writable)
    .catch(() => sink.teardown());
  return { canvas: sink.canvas, drop: sink.teardown, stats: sink.stats };
}

/**
 * Direct video delivery over the nanoctl.video.v1 unreliable data channel:
 * each message is one Annex-B access unit — no jitter buffer at all.
 */
export function attachDataChannelVideo(
  channel: RTCDataChannel,
  idrChannel: RTCDataChannel | null,
  video: HTMLVideoElement,
): WebCodecsVideoHandle {
  const sink = createVideoSink(video);
  // Wire format: [u32 seq][u8 flags][annex-b au]. Deltas ride the unordered,
  // unreliable channel; keyframes (large, loss-fatal) ride the reliable
  // `nanoctl.video-idr.v1` channel. A gap or reorder breaks the reference
  // chain — drop deltas until the next keyframe and ask the host for one.
  let expectedSeq: number | null = null;
  let waitingForKey = true;
  let kfRequestedAt = 0;
  const dbg = ((
    window as unknown as { __dcv?: Record<string, number | boolean | string> }
  ).__dcv ??= { msgs: 0, fed: 0, gaps: 0, nonArrayBuffer: 0, kf: 0 });
  dbg.waitingForKey = true;
  const pending = new Map<
    number,
    { isKey: boolean; parts: number; got: Uint8Array[]; n: number }
  >();
  const reordered = new Map<number, { au: Uint8Array; isKey: boolean }>();
  const onFrame = (event: MessageEvent) => {
    try {
      const data = event.data instanceof ArrayBuffer ? new Uint8Array(event.data) : null;
      if (!data || data.length < 18) {
        dbg.nonArrayBuffer = (dbg.nonArrayBuffer as number) + 1;
        return;
      }
      dbg.msgs = (dbg.msgs as number) + 1;
      const seq = data[0]! | (data[1]! << 8) | (data[2]! << 16) | (data[3]! << 24);
      const isKey = (data[4]! & 1) !== 0;
      const part = data[5]! | (data[6]! << 8);
      const parts = data[7]! | (data[8]! << 8);
      const dv = new DataView(data.buffer, data.byteOffset + 9);
      const sentMs = Number(dv.getBigUint64(0, true));
      const transitMs = Date.now() - sentMs;
      dbg.transitMs = transitMs;
      dbg.transitSum = ((dbg.transitSum as number) || 0) + transitMs;
      const chunk = data.subarray(17);
      let au: Uint8Array | null = chunk;
      if (parts > 1) {
        let entry = pending.get(seq);
        if (!entry) {
          entry = { isKey, parts, got: [], n: 0 };
          entry.got.length = parts;
          pending.set(seq, entry);
        }
        if (!entry.got[part]) {
          entry.got[part] = chunk;
          entry.n += 1;
        }
        if (entry.n < parts) return;
        pending.delete(seq);
        const total = entry.got.reduce((a, b) => a + b.length, 0);
        const joined = new Uint8Array(total);
        let off = 0;
        for (const g of entry.got) {
          joined.set(g, off);
          off += g.length;
        }
        au = joined;
        // Drop stale partials that can never complete.
        for (const key of pending.keys()) {
          if (((seq - key) | 0) > 0) pending.delete(key);
        }
      }
      if (!au) return;
      if (expectedSeq === null) {
        expectedSeq = seq;
      }
      const diff = (seq - expectedSeq) | 0;
      if (diff < 0) {
        dbg.late = ((dbg.late as number) || 0) + 1;
        return; // Already delivered (or already declared lost).
      }
      let deliverNow = diff === 0;
      if (diff > 0 && isKey && waitingForKey) {
        // A keyframe always jumps the reorder window — the missing seq is
        // declared lost and only this IDR can restart the chain.
        deliverNow = true;
      } else if (diff > 0) {
        // Unordered delivery reorders constantly; hold ahead-of-order frames
        // in a small reorder window instead of declaring a gap. While waiting
        // for a key only keys are worth holding.
        if (isKey || !waitingForKey) {
          reordered.set(seq, { au, isKey });
          dbg.reordered = ((dbg.reordered as number) || 0) + 1;
        }
        if (reordered.size > 16 && !waitingForKey) {
          waitingForKey = true;
          dbg.gaps = (dbg.gaps as number) + 1;
          reordered.clear();
        }
      }
      // Drain the reorder window in sequence.
      const deliver = (s: number, a: Uint8Array, k: boolean) => {
        expectedSeq = (s + 1) >>> 0;
        if (waitingForKey) {
          if (Date.now() - kfRequestedAt > 250) {
            kfRequestedAt = Date.now();
            dbg.kf = (dbg.kf as number) + 1;
            try {
              channel.send("KF");
            } catch {}
          }
          if (!k) return;
          waitingForKey = false;
          dbg.waitingForKey = false;
        }
        dbg.fed = (dbg.fed as number) + 1;
        sink.feed(a, k, s * 16_000);
      };
      if (deliverNow) {
        deliver(seq, au, isKey);
      }
      while (expectedSeq !== null && reordered.has(expectedSeq)) {
        const e = reordered.get(expectedSeq)!;
        reordered.delete(expectedSeq);
        deliver(expectedSeq, e.au, e.isKey);
      }
      dbg.waitingForKey = waitingForKey;
      const st = sink.stats();
      dbg.decoded = st.decoded;
      dbg.decodeDropped = st.dropped;
      return;
    } catch {
      // Decoder must never break the channel.
    }
  };
  channel.onmessage = onFrame;
  if (idrChannel) idrChannel.onmessage = onFrame;
  channel.onclose = () => sink.teardown();
  if (idrChannel) idrChannel.onclose = () => sink.teardown();
  return { canvas: sink.canvas, drop: sink.teardown, stats: sink.stats };
}
