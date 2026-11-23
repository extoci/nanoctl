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
export function createVideoEncodedStreams(
  receiver: RTCRtpReceiver,
): EncodedStreams | null {
  try {
    return (
      receiver as RTCRtpReceiver & { createEncodedStreams?: () => EncodedStreams }
    ).createEncodedStreams?.() ?? null;
  } catch {
    return null;
  }
}

export function attachWebCodecsVideo(
  streams: EncodedStreams,
  video: HTMLVideoElement,
): WebCodecsVideoHandle | null {
  const canvas = document.createElement("canvas");
  canvas.className = "webcodecs-video";
  canvas.setAttribute("aria-hidden", "true");
  video.insertAdjacentElement("afterend", canvas);
  const ctx = canvas.getContext("2d", { alpha: false });

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
          codec = `avc1.${hex(annexB[i + 4]!)}${hex(annexB[i + 5]!)}${hex(
            annexB[i + 6]!,
          )}`;
          break;
        }
      }
    }
    const next = new VideoDecoder({
      output: (frame) => {
        try {
          if (ctx) {
            if (
              canvas.width !== frame.displayWidth ||
              canvas.height !== frame.displayHeight
            ) {
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
      next.configure({ codec, optimizeForLatency: true });
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

  const containsIdr = (annexB: Uint8Array): boolean => {
    for (let i = 0; i + 4 < annexB.length; i++) {
      if (annexB[i] === 0 && annexB[i + 1] === 0 && annexB[i + 2] === 1) {
        const type = annexB[i + 3]! & 0x1f;
        if (type === 5 || type === 7) return true;
      }
    }
    return false;
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
    if (decoder.decodeQueueSize > 4) {
      if (!isKey) {
        dropped += 1;
        return;
      }
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

  const wrappedTransform = new TransformStream<EncodedVideoFrameLike, EncodedVideoFrameLike>({
    transform: (frame, controller) => {
      try {
        const rtpTimestamp = frame.getMetadata().rtpTimestamp ?? 0;
        const data = new Uint8Array(frame.data);
        const isKey = frame.type === "key" || containsIdr(data);
        feed(data, isKey, Math.round(rtpTimestamp * (1e6 / 90_000)));
      } catch {
        // Tap must never break the normal pipeline.
      }
      controller.enqueue(frame);
    },
  });

  const teardown = () => {
    if (closed) return;
    closed = true;
    try {
      decoder?.close();
    } catch {}
    canvas.remove();
  };

  streams.readable.pipeThrough(wrappedTransform).pipeTo(streams.writable).catch(
    () => {
      // Track/PC closed mid-pipe — fall back to the plain <video> path.
      teardown();
    },
  );

  return {
    canvas,
    drop: teardown,
    stats: () => ({ decoded, dropped }),
  };
}
