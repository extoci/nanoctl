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

export type DirectVideoCodec = "h264" | "hevc" | "av1";

/**
 * True when an Annex-B access unit starts a random-access picture. Every
 * slice of a picture shares its keyframe-ness and parameter sets precede the
 * first slice, so the scan stops at the first VCL NAL instead of walking the
 * whole (often tens of KB) slice payload of every delta frame.
 */
export function containsIdr(annexB: Uint8Array, codec: DirectVideoCodec = "h264"): boolean {
  if (codec === "av1") return av1IsRandomAccess(annexB);
  for (let i = 0; i + 4 < annexB.length; i++) {
    if (annexB[i] === 0 && annexB[i + 1] === 0 && annexB[i + 2] === 1) {
      if (codec === "h264") {
        const type = annexB[i + 3]! & 0x1f;
        if (type === 5 || type === 7) return true;
        if (type >= 1 && type <= 4) return false;
      } else {
        const type = (annexB[i + 3]! >> 1) & 0x3f;
        // BLA/IDR/CRA pictures and the VPS/SPS/PPS that precede them.
        if ((type >= 16 && type <= 21) || (type >= 32 && type <= 34)) return true;
        if (type <= 18) return false;
      }
      i += 3;
    }
  }
  return false;
}

function stripEmulationPrevention(bytes: Uint8Array): Uint8Array {
  const out = new Uint8Array(bytes.length);
  let n = 0;
  for (let i = 0; i < bytes.length; i++) {
    if (i + 2 < bytes.length && bytes[i] === 0 && bytes[i + 1] === 0 && bytes[i + 2] === 3) {
      out[n++] = 0;
      out[n++] = 0;
      i += 2;
    } else {
      out[n++] = bytes[i]!;
    }
  }
  return out.subarray(0, n);
}

class BitReader {
  private bit = 0;

  constructor(private readonly bytes: Uint8Array) {}

  read(width: number): number | null {
    let value = 0;
    for (let i = 0; i < width; i++) {
      const byte = this.bytes[this.bit >> 3];
      if (byte === undefined) return null;
      value = (value << 1) | ((byte >> (7 - (this.bit & 7))) & 1);
      this.bit += 1;
    }
    return value >>> 0;
  }

  skip(width: number): boolean {
    if (this.bit + width > this.bytes.length * 8) return false;
    this.bit += width;
    return true;
  }

  readUvlc(): number | null {
    let leadingZeros = 0;
    while (leadingZeros < 32) {
      const bit = this.read(1);
      if (bit === null) return null;
      if (bit !== 0) break;
      leadingZeros += 1;
    }
    if (leadingZeros === 32) return null;
    const suffix = leadingZeros === 0 ? 0 : this.read(leadingZeros);
    if (suffix === null) return null;
    return (1 << leadingZeros) - 1 + suffix;
  }
}

/**
 * Derives the fully-qualified HEVC codec string from the profile_tier_level
 * at the start of an SPS NAL. HEVC's Annex-B wire format carries that SPS
 * in-band, so no out-of-band HEVCDecoderConfigurationRecord is needed.
 */
export function hevcCodecOf(annexB: Uint8Array): string {
  const fallback = "hev1.1.6.L153.B0";
  for (let i = 0; i + 5 < annexB.length; i++) {
    if (!(annexB[i] === 0 && annexB[i + 1] === 0 && annexB[i + 2] === 1)) continue;
    const header = i + 3;
    if (((annexB[header]! >> 1) & 0x3f) !== 33) {
      i += 3;
      continue;
    }
    let end = annexB.length;
    for (let j = header + 2; j + 2 < annexB.length; j++) {
      if (annexB[j] === 0 && annexB[j + 1] === 0 && annexB[j + 2] === 1) {
        end = j;
        break;
      }
    }
    const rbsp = stripEmulationPrevention(annexB.subarray(header + 2, end));
    const bits = new BitReader(rbsp);
    if (!bits.skip(8)) return fallback; // vps id, max sub layers, temporal nesting
    const profileSpace = bits.read(2);
    const tier = bits.read(1);
    const profileIdc = bits.read(5);
    const rawCompat = bits.read(32);
    if (profileSpace === null || tier === null || profileIdc === null || rawCompat === null) {
      return fallback;
    }
    let compat = 0;
    for (let flag = 0; flag < 32; flag++) {
      if ((rawCompat & (1 << (31 - flag))) !== 0) compat |= 1 << flag;
    }
    compat >>>= 0;
    const constraint: number[] = [];
    for (let byte = 0; byte < 6; byte++) {
      const value = bits.read(8);
      if (value === null) return fallback;
      constraint.push(value);
    }
    const level = bits.read(8);
    if (level === null) return fallback;
    let lastConstraint = constraint.length - 1;
    while (lastConstraint > 0 && constraint[lastConstraint] === 0) lastConstraint -= 1;
    const constraintText = constraint
      .slice(0, lastConstraint + 1)
      .map((value) => value.toString(16).padStart(2, "0").toUpperCase())
      .join(".");
    const space = profileSpace === 0 ? "" : String.fromCharCode(64 + profileSpace);
    return `hev1.${space}${profileIdc}.${compat.toString(16).toUpperCase()}.${
      tier === 0 ? "L" : "H"
    }${level}.${constraintText}`;
  }
  return fallback;
}

interface Av1Obu {
  type: number;
  payload: Uint8Array;
}

/** Iterates low-overhead AV1 OBUs, the same temporal-unit format WebCodecs consumes. */
function av1Obus(data: Uint8Array): Av1Obu[] {
  const out: Av1Obu[] = [];
  let offset = 0;
  while (offset < data.length) {
    const header = data[offset++]!;
    if ((header & 0x80) !== 0 || (header & 0x02) === 0) break;
    const type = (header >> 3) & 0x0f;
    if ((header & 0x04) !== 0) offset += 1;
    let size = 0;
    let shift = 0;
    for (;;) {
      const byte = data[offset++];
      if (byte === undefined) return out;
      size += (byte & 0x7f) * 2 ** shift;
      if ((byte & 0x80) === 0) break;
      shift += 7;
      if (shift > 28) return out;
    }
    const end = offset + size;
    if (end > data.length) break;
    out.push({ type, payload: data.subarray(offset, end) });
    offset = end;
  }
  return out;
}

function av1IsRandomAccess(data: Uint8Array): boolean {
  let sequence = false;
  let key = false;
  let frames = 0;
  for (const obu of av1Obus(data)) {
    if (obu.type === 1) sequence = true;
    if (obu.type === 3 || obu.type === 6) {
      frames += 1;
      // show_existing_frame = 0 followed by frame_type = KEY_FRAME (0),
      // matching the agent's keyframe classifier.
      if ((obu.payload[0]! & 0xe0) === 0) key = true;
    }
  }
  return sequence && key && frames > 0;
}

/**
 * Derives the AV1 codec string from the first operating point in the sequence
 * header. nanoctl currently only negotiates Main 4:2:0 8-bit AV1, so the
 * bit-depth suffix is fixed to 08 rather than parsing the full color config.
 */
export function av1CodecOf(data: Uint8Array): string {
  const fallback = "av01.0.08M.08";
  const sequence = av1Obus(data).find((obu) => obu.type === 1);
  if (!sequence) return fallback;
  const bits = new BitReader(sequence.payload);
  const profile = bits.read(3);
  if (profile === null || !bits.skip(2)) return fallback;
  let level: number | null;
  let tier = 0;
  const reduced = (sequence.payload[0]! & 0x08) !== 0;
  if (reduced) {
    level = bits.read(5);
  } else {
    const timingInfoPresent = bits.read(1);
    if (timingInfoPresent === null) return fallback;
    let decoderModelInfoPresent = 0;
    if (timingInfoPresent !== 0) {
      if (!bits.skip(64)) return fallback;
      const equalPictureInterval = bits.read(1);
      if (equalPictureInterval === null) return fallback;
      if (equalPictureInterval !== 0 && bits.readUvlc() === null) return fallback;
      const present = bits.read(1);
      if (present === null) return fallback;
      decoderModelInfoPresent = present;
      if (decoderModelInfoPresent !== 0 && !bits.skip(5 + 32 + 5 + 5)) return fallback;
    }
    if (!bits.skip(1)) return fallback; // initial_display_delay_present
    if (!bits.skip(5 + 12)) return fallback; // op count + first operating point idc
    level = bits.read(5);
    if (level !== null && level > 7) {
      const value = bits.read(1);
      if (value === null) return fallback;
      tier = value;
    }
  }
  if (profile === null || profile > 2 || level === null || level > 31) return fallback;
  return `av01.${profile}.${String(level).padStart(2, "0")}${tier === 0 ? "M" : "H"}.08`;
}

interface VideoSink {
  canvas: HTMLCanvasElement;
  feed(data: Uint8Array, isKey: boolean, timestampUs: number): boolean;
  teardown(notify?: boolean): void;
  stats(): { decoded: number; dropped: number };
}

interface VideoTrackGeneratorLike {
  track: MediaStreamTrack;
  writable: WritableStream<VideoFrame>;
}

function createVideoSink(
  video: HTMLVideoElement,
  onTeardown?: () => void,
  joinAt?: number,
  videoCodec: DirectVideoCodec = "h264",
): VideoSink {
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
  let generatorStream: MediaStream | null = null;
  if (generator) {
    // Older builds expose `.track`; current Chrome makes the generator itself
    // a MediaStreamTrack subclass.
    const track = generator.track ?? (generator as unknown as MediaStreamTrack);
    generatorStream = new MediaStream([track]);
    video.srcObject = generatorStream;
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
  // Decode latency probe: chunk timestampUs -> performance.now() at decode().
  const decodeSubmit = new Map<number, number>();
  // Present latency probe: decoder-output times keyed by frame timestamp.
  // rVFC mediaTime is seconds; its timeline base and fractional precision
  // vary, so match the nearest decoded-frame timestamp within 5ms instead
  // of an exact key.
  const decodeOutput = new Map<number, number>();
  const armPresentProbe = () => {
    if (
      typeof (video as { requestVideoFrameCallback?: unknown }).requestVideoFrameCallback !==
      "function"
    ) {
      return;
    }
    const onFrame = (now: number, meta: { mediaTime?: number }) => {
      const dbg = ((window as unknown as { __dcv?: Record<string, unknown> }).__dcv ??= {});
      if (meta.mediaTime !== undefined && decodeOutput.size > 0) {
        const stampUs = Math.round(meta.mediaTime * 1e6);
        let bestKey = 0;
        let bestDiff = Number.POSITIVE_INFINITY;
        for (const key of decodeOutput.keys()) {
          const diff = Math.abs(key - stampUs);
          if (diff < bestDiff) {
            bestDiff = diff;
            bestKey = key;
          }
        }
        if (bestDiff <= 5000) {
          const out = decodeOutput.get(bestKey)!;
          decodeOutput.delete(bestKey);
          const ms = now - out;
          dbg.presentSum = ((dbg.presentSum as number) ?? 0) + ms;
          dbg.presentN = ((dbg.presentN as number) ?? 0) + 1;
          dbg.presentMax = Math.max((dbg.presentMax as number) ?? 0, ms);
        }
      }
      if (!closed) {
        (
          video as HTMLVideoElement & {
            requestVideoFrameCallback: (cb: typeof onFrame) => void;
          }
        ).requestVideoFrameCallback(onFrame);
      }
    };
    (
      video as HTMLVideoElement & {
        requestVideoFrameCallback: (cb: typeof onFrame) => void;
      }
    ).requestVideoFrameCallback(onFrame);
  };
  armPresentProbe();

  // Derive the codec string from parameter sets in an access unit.
  const codecOf = (bitstream: Uint8Array): string => {
    if (videoCodec === "av1") return av1CodecOf(bitstream);
    if (videoCodec === "hevc") return hevcCodecOf(bitstream);
    let codec = "avc1.640028";
    for (let i = 0; i + 7 < bitstream.length; i++) {
      if (bitstream[i] === 0 && bitstream[i + 1] === 0 && bitstream[i + 2] === 1) {
        const nal = bitstream[i + 3]!;
        if ((nal & 0x1f) === 7 && i + 7 < bitstream.length) {
          const hex = (b: number) => b.toString(16).padStart(2, "0");
          codec = `avc1.${hex(bitstream[i + 4]!)}${hex(bitstream[i + 5]!)}${hex(bitstream[i + 6]!)}`;
          break;
        }
      }
    }
    return codec;
  };

  const decoderConfig = (codec: string) =>
    ({
      codec,
      optimizeForLatency: true,
      latencyMode: "realtime",
    }) as VideoDecoderConfig & { latencyMode: "realtime" };

  let configuredCodec: string | undefined;

  const ensureDecoder = (annexB: Uint8Array) => {
    if (decoder || typeof VideoDecoder !== "function") return;
    const codec = codecOf(annexB);
    const next = new VideoDecoder({
      output: (frame) => {
        const dbg = (window as unknown as { __dcv?: Record<string, unknown> }).__dcv;
        if (dbg && joinAt !== undefined && dbg.firstDecodeMs === undefined) {
          dbg.firstDecodeMs = performance.now() - joinAt;
        }
        const submit = decodeSubmit.get(frame.timestamp);
        if (submit !== undefined) {
          decodeSubmit.delete(frame.timestamp);
          const ms = performance.now() - submit;
          if (dbg) {
            dbg.decodeSum = ((dbg.decodeSum as number) ?? 0) + ms;
            dbg.decodeN = ((dbg.decodeN as number) ?? 0) + 1;
            dbg.decodeMax = Math.max((dbg.decodeMax as number) ?? 0, ms);
          }
        }
        if (decodeSubmit.size > 64) decodeSubmit.clear();
        decodeOutput.set(frame.timestamp, performance.now());
        if (decodeOutput.size > 256) {
          decodeOutput.delete(decodeOutput.keys().next().value!);
        }
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
        teardown(true);
      },
    });
    try {
      next.configure(decoderConfig(codec));
      decoder = next;
      configuredCodec = codec;
    } catch (error) {
      console.warn("webcodecs video configure failed", error);
      teardown(true);
    }
  };

  // Receiver-side encoded-transform frames are already depacketized AUs;
  // Chrome delivers H.264 in Annex-B. Keep an AVCC/packetized fallback just in
  // case the framing differs.
  let decoderConfigured = false;
  let resyncing = false;

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

  const feed = (data: Uint8Array, isKey: boolean, timestampUs: number): boolean => {
    const bitstream = videoCodec === "av1" ? data : toAnnexB(data);
    if (!decoderConfigured) {
      if (!isKey && !containsIdr(bitstream, videoCodec)) {
        dropped += 1;
        return false;
      }
      ensureDecoder(bitstream);
      if (!decoder) return false;
      decoderConfigured = true;
    }
    if (!decoder) return false;
    const key = isKey || containsIdr(bitstream, videoCodec);
    if (resyncing && !key) {
      dropped += 1;
      return false;
    }
    // Adaptive encoders may switch profiles between random-access pictures.
    // Bitstreams carry the new sequence parameters in-band; re-configure the
    // decoder so the codec string tracks them. A recovery keyframe also resets
    // the decoder: its queue may still contain deltas whose reference chain
    // was deliberately abandoned.
    if (key) {
      const codec = codecOf(bitstream);
      if (resyncing || codec !== configuredCodec) {
        try {
          decoder.reset();
          decoder.configure(decoderConfig(codec));
          configuredCodec = codec;
          resyncing = false;
        } catch {
          decoderConfigured = false;
          return false;
        }
      }
    }
    if (decoder.decodeQueueSize > 1 && !key) {
      // Never decode deltas after a local drop: their reference chain is broken
      // and decoding them produces persistent smearing. Drop forward to the
      // next keyframe instead; the data-channel caller requests one.
      dropped += 1;
      resyncing = true;
      return false;
    }
    try {
      decodeSubmit.set(timestampUs, performance.now());
      decoder.decode(
        new EncodedVideoChunk({
          type: key ? "key" : "delta",
          timestamp: timestampUs,
          data: bitstream,
        }),
      );
      return true;
    } catch {
      decoderConfigured = false;
      resyncing = true;
      return false;
    }
  };

  const teardown = (notify = false) => {
    if (closed) return;
    closed = true;
    try {
      decoder?.close();
    } catch {}
    if (generator) {
      void writer?.close().catch(() => {});
      (generator.track ?? (generator as unknown as MediaStreamTrack)).stop();
      if (generatorStream && video.srcObject === generatorStream) {
        video.srcObject = null;
      }
    }
    canvas?.remove();
    if (notify) onTeardown?.();
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
  onTeardown?: () => void,
): WebCodecsVideoHandle {
  const sink = createVideoSink(video, onTeardown, performance.now());
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
    .catch(() => sink.teardown(true));
  return { canvas: sink.canvas, drop: () => sink.teardown(false), stats: sink.stats };
}

/**
 * Direct video delivery over the nanoctl.video.v1 unordered bounded-retransmit data channel:
 * each message is one compressed access unit — no jitter buffer at all.
 */
export function attachDataChannelVideo(
  channel: RTCDataChannel,
  idrChannel: RTCDataChannel | null,
  video: HTMLVideoElement,
  onTeardown?: () => void,
  videoCodec: DirectVideoCodec = "h264",
): WebCodecsVideoHandle {
  // Join latency: attach -> first keyframe fed to the decoder.
  const joinAt = performance.now();
  const sink = createVideoSink(video, onTeardown, joinAt, videoCodec);
  // Wire format: [u32 seq][u8 flags][access-unit bytes]. Deltas ride the unordered,
  // bounded-retransmit channel; keyframes (large, loss-fatal) ride the reliable
  // `nanoctl.video-idr.v1` channel. A gap or reorder breaks the reference
  // chain — drop deltas until the next keyframe and ask the host for one.
  let expectedSeq: number | null = null;
  let waitingForKey = true;
  let kfRequestedAt = 0;
  const dbg = ((
    window as unknown as { __dcv?: Record<string, number | boolean | string> }
  ).__dcv ??= { msgs: 0, fed: 0, gaps: 0, nonArrayBuffer: 0, kf: 0 });
  dbg.waitingForKey = true;
  dbg.joinAt = joinAt;
  delete dbg.joinMs;
  delete dbg.firstMsgMs;
  delete dbg.firstDecodeMs;
  const requestKeyframe = () => {
    if (Date.now() - kfRequestedAt <= 250) return;
    kfRequestedAt = Date.now();
    dbg.kf = (dbg.kf as number) + 1;
    try {
      // The bounded-retransmit delta channel can still drop this one-byte recovery request
      // exactly when loss recovery is needed. The IDR channel is reliable and
      // the host accepts the request on either channel.
      (idrChannel ?? channel).send("KF");
    } catch {}
  };
  // Ask for a keyframe immediately: without it the request only goes out
  // after the first delta arrives, and an idle producer can delay that.
  if (!(window as unknown as { __noEarlyKf?: boolean }).__noEarlyKf) {
    requestKeyframe();
  }
  const pending = new Map<
    number,
    { isKey: boolean; parts: number; got: Uint8Array[]; n: number }
  >();
  const reordered = new Map<number, { au: Uint8Array; isKey: boolean }>();
  const onFrame = (event: MessageEvent) => {
    try {
      const data = event.data instanceof ArrayBuffer ? new Uint8Array(event.data) : null;
      if (!data || data.length < 6) {
        dbg.nonArrayBuffer = (dbg.nonArrayBuffer as number) + 1;
        return;
      }
      dbg.msgs = (dbg.msgs as number) + 1;
      dbg.bytes = ((dbg.bytes as number) || 0) + data.byteLength;
      if (dbg.firstMsgMs === undefined) {
        dbg.firstMsgMs = performance.now() - joinAt;
      }
      const seq = data[0]! | (data[1]! << 8) | (data[2]! << 16) | (data[3]! << 24);
      const flags = data[4]!;
      const isKey = (flags & 1) !== 0;
      let offset = 5;
      let part = 0;
      let parts = 1;
      if ((flags & 2) !== 0) {
        if (data.length < offset + 5) return;
        part = data[offset]! | (data[offset + 1]! << 8);
        parts = data[offset + 2]! | (data[offset + 3]! << 8);
        offset += 4;
        if (part >= parts) return;
      }
      if ((flags & 4) !== 0) {
        if (data.length < offset + 8) return;
        const dv = new DataView(data.buffer, data.byteOffset + offset);
        const transitMs = Date.now() - Number(dv.getBigUint64(0, true));
        dbg.transitMs = transitMs;
        dbg.transitSum = ((dbg.transitSum as number) || 0) + transitMs;
        offset += 8;
      }
      if (data.length <= offset) return;
      const chunk = data.subarray(offset);
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
      }
      // Drop stale partials that can never complete. An incomplete delta
      // means the reference chain is already broken, so start keyframe
      // recovery now instead of waiting for the reorder window to overflow.
      let lostIncomplete = false;
      for (const key of pending.keys()) {
        if (((seq - key) | 0) > 0) {
          pending.delete(key);
          lostIncomplete = true;
        }
      }
      if (lostIncomplete && !waitingForKey) {
        waitingForKey = true;
        dbg.gaps = (dbg.gaps as number) + 1;
        reordered.clear();
        requestKeyframe();
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
      if (diff > 0 && isKey) {
        // An IDR does not reference any missing predecessor, so it always
        // jumps the reorder window. Older reordered deltas are both undecodable
        // and temporally stale once this newer keyframe exists.
        deliverNow = true;
        for (const buffered of reordered.keys()) {
          if (((buffered - seq) | 0) < 0) reordered.delete(buffered);
        }
      } else if (diff > 0) {
        // Unordered delivery reorders constantly; hold ahead-of-order frames
        // in a small reorder window instead of declaring a gap. While waiting
        // for a key only keys are worth holding.
        if (isKey || !waitingForKey) {
          reordered.set(seq, { au, isKey });
          dbg.reordered = ((dbg.reordered as number) || 0) + 1;
        }
        if (reordered.size > 4 && !waitingForKey) {
          waitingForKey = true;
          dbg.gaps = (dbg.gaps as number) + 1;
          reordered.clear();
          // Nothing can drain while the missing sequence is absent, so asking
          // here rather than in deliver() is what bounds recovery time.
          requestKeyframe();
        }
      }
      // Drain the reorder window in sequence.
      const deliver = (s: number, a: Uint8Array, k: boolean) => {
        expectedSeq = (s + 1) >>> 0;
        if (waitingForKey) {
          requestKeyframe();
          if (!k) return;
          waitingForKey = false;
          kfRequestedAt = 0;
          dbg.waitingForKey = false;
          dbg.joinMs = performance.now() - joinAt;
        }
        dbg.fed = (dbg.fed as number) + 1;
        const accepted = sink.feed(a, k, s * 16_000);
        if (!accepted && !k && !waitingForKey) {
          waitingForKey = true;
          dbg.waitingForKey = true;
          dbg.gaps = (dbg.gaps as number) + 1;
          reordered.clear();
          requestKeyframe();
        }
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
  channel.onclose = () => sink.teardown(true);
  if (idrChannel) idrChannel.onclose = () => sink.teardown(true);
  return { canvas: sink.canvas, drop: () => sink.teardown(false), stats: sink.stats };
}
