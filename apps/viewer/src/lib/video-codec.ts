import type { VideoCodec } from "@nanoctl/protocol";

export type CodecChoice = "auto" | Exclude<VideoCodec, "vp9">;

export const CODEC_CHOICES: readonly { value: CodecChoice; label: string }[] = [
  { value: "auto", label: "Auto" },
  { value: "av1", label: "AV1" },
  { value: "hevc", label: "HEVC" },
  { value: "h264", label: "H.264" },
];

const STORAGE_KEY = "video-codec";
const MIME: Record<Exclude<CodecChoice, "auto">, string> = {
  av1: "video/av1",
  hevc: "video/h265",
  h264: "video/h264",
};
const CONTENT_TYPE: Record<Exclude<CodecChoice, "auto">, string> = {
  av1: "video/AV1",
  hevc: "video/H265",
  h264: "video/H264",
};
/** Highest compression efficiency first. */
const BY_EFFICIENCY = ["av1", "hevc", "h264"] as const;

export function loadCodecChoice(storage: Pick<Storage, "getItem"> = localStorage): CodecChoice {
  const stored = storage.getItem(STORAGE_KEY);
  return CODEC_CHOICES.some((choice) => choice.value === stored) ? (stored as CodecChoice) : "auto";
}

export function saveCodecChoice(
  choice: CodecChoice,
  storage: Pick<Storage, "setItem"> = localStorage,
): void {
  storage.setItem(STORAGE_KEY, choice);
}

export type DecodeSupport = (codec: (typeof BY_EFFICIENCY)[number]) => Promise<{
  supported: boolean;
  powerEfficient: boolean;
}>;

/** Asks the browser whether a WebRTC stream decodes in hardware (Moonlight's auto rule). */
export const browserDecodeSupport: DecodeSupport = async (codec) => {
  const capabilities = navigator.mediaCapabilities;
  if (!capabilities?.decodingInfo) return { supported: codec === "h264", powerEfficient: false };
  try {
    const info = await capabilities.decodingInfo({
      type: "webrtc",
      video: {
        contentType: CONTENT_TYPE[codec],
        width: 3840,
        height: 2160,
        bitrate: 50_000_000,
        framerate: 60,
      },
    } as MediaDecodingConfiguration);
    return { supported: info.supported, powerEfficient: info.powerEfficient };
  } catch {
    return { supported: codec === "h264", powerEfficient: false };
  }
};

/**
 * Orders the browser's receive codecs so the host picks the best codec both sides support.
 * Auto prefers the most efficient codec the browser decodes in hardware: a software AV1
 * decode of a 4K desktop costs more latency than the bitrate it saves. An explicit choice
 * moves that codec first; the host still falls back to H.264 when it cannot encode it.
 */
export async function orderVideoCodecs(
  choice: CodecChoice,
  hostCodecs: readonly VideoCodec[],
  receiveCodecs: readonly RTCRtpCodec[],
  decodeSupport: DecodeSupport = browserDecodeSupport,
): Promise<RTCRtpCodec[] | null> {
  const offered = BY_EFFICIENCY.filter(
    (codec) =>
      hostCodecs.includes(codec) &&
      receiveCodecs.some((receive) => receive.mimeType.toLowerCase() === MIME[codec]),
  );
  if (offered.length === 0) return null;
  let order: (typeof BY_EFFICIENCY)[number][];
  if (choice !== "auto" && offered.includes(choice)) {
    order = [choice, ...offered.filter((codec) => codec !== choice)];
  } else {
    const support = await Promise.all(offered.map((codec) => decodeSupport(codec)));
    const hardware = offered.filter((_, index) => support[index]?.powerEfficient);
    order = [
      ...hardware,
      ...(offered.includes("h264") && !hardware.includes("h264") ? ["h264" as const] : []),
      ...offered.filter((codec) => !hardware.includes(codec) && codec !== "h264"),
    ];
  }
  const rank = (codec: RTCRtpCodec) => {
    const index = order.findIndex((name) => MIME[name] === codec.mimeType.toLowerCase());
    return index === -1 ? order.length : index;
  };
  return receiveCodecs
    .map((codec, index) => ({ codec, index }))
    .sort((a, b) => rank(a.codec) - rank(b.codec) || a.index - b.index)
    .map(({ codec }) => codec);
}

export function codecLabel(mimeType: string | undefined): string | null {
  if (!mimeType) return null;
  const subtype = mimeType.split("/")[1]?.toLowerCase();
  return subtype === "h265"
    ? "HEVC"
    : subtype === "h264"
      ? "H.264"
      : (subtype?.toUpperCase() ?? null);
}
