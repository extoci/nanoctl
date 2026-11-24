import { describe, expect, test } from "bun:test";
import { codecLabel, loadCodecChoice, orderVideoCodecs, type DecodeSupport } from "./video-codec";

const receive: RTCRtpCodec[] = [
  { mimeType: "video/VP8", clockRate: 90_000 },
  { mimeType: "video/rtx", clockRate: 90_000 },
  {
    mimeType: "video/H264",
    clockRate: 90_000,
    sdpFmtpLine: "packetization-mode=1",
  },
  { mimeType: "video/AV1", clockRate: 90_000 },
  { mimeType: "video/H265", clockRate: 90_000 },
];
const mimes = (codecs: RTCRtpCodec[] | null) => codecs?.map((codec) => codec.mimeType);
const hardware =
  (...codecs: string[]): DecodeSupport =>
  async (codec) => ({
    supported: true,
    powerEfficient: codecs.includes(codec),
  });

describe("video codec preferences", () => {
  test("auto prefers the most efficient hardware-decoded codec the host encodes", async () => {
    const all = ["h264", "hevc", "av1"] as const;
    expect(mimes(await orderVideoCodecs("auto", all, receive, hardware("av1", "hevc")))).toEqual([
      "video/AV1",
      "video/H265",
      "video/H264",
      "video/VP8",
      "video/rtx",
    ]);
    expect(
      mimes(await orderVideoCodecs("auto", all, receive, hardware("hevc")))?.slice(0, 3),
    ).toEqual(["video/H265", "video/H264", "video/AV1"]);
    expect(mimes(await orderVideoCodecs("auto", all, receive, hardware()))?.[0]).toBe("video/H264");
    expect(mimes(await orderVideoCodecs("auto", ["h264"], receive, hardware("av1")))?.[0]).toBe(
      "video/H264",
    );
  });

  test("an explicit choice goes first only when both sides support it", async () => {
    const all = ["h264", "hevc", "av1"] as const;
    expect(mimes(await orderVideoCodecs("av1", all, receive, hardware()))?.[0]).toBe("video/AV1");
    expect(mimes(await orderVideoCodecs("hevc", ["h264"], receive, hardware()))?.[0]).toBe(
      "video/H264",
    );
    expect(await orderVideoCodecs("av1", ["vp9"], receive, hardware())).toBeNull();
  });

  test("stored choices are validated", () => {
    expect(loadCodecChoice({ getItem: () => "av1" })).toBe("av1");
    expect(loadCodecChoice({ getItem: () => "vp9" })).toBe("auto");
    expect(loadCodecChoice({ getItem: () => null })).toBe("auto");
    expect(codecLabel("video/H265")).toBe("HEVC");
    expect(codecLabel("video/AV1")).toBe("AV1");
    expect(codecLabel(undefined)).toBeNull();
  });
});
