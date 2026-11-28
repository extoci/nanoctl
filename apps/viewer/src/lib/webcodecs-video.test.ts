import { describe, expect, test } from "bun:test";
import { av1CodecOf, containsIdr, hevcCodecOf } from "./webcodecs-video";

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

describe("HEVC Annex-B support", () => {
  const hevcNal = (type: number, payload: number[] = [0x01]) => [0, 0, 0, 1, type << 1, ...payload];

  test("detects HEVC random-access NALs and stops at trailing slices", () => {
    expect(containsIdr(new Uint8Array(hevcNal(19)), "hevc")).toBe(true);
    expect(containsIdr(new Uint8Array(hevcNal(32)), "hevc")).toBe(true);
    expect(containsIdr(new Uint8Array(hevcNal(1)), "hevc")).toBe(false);
    expect(containsIdr(new Uint8Array([...hevcNal(1), 0, 0, 1, 19 << 1]), "hevc")).toBe(false);
  });

  test("derives the codec string from an emulation-prevented SPS", () => {
    // x265 Main Intra SPS, including 00 00 03 emulation-prevention sequences.
    const sps = [
      0x42, 0x01, 0x01, 0x04, 0x08, 0x00, 0x00, 0x03, 0x00, 0x9f, 0xa8, 0x00, 0x00, 0x03, 0x00,
      0x00, 0x3c, 0xa0, 0x0a, 0x08, 0x0f, 0x16, 0x5b, 0xa9, 0x24, 0xca, 0xf0, 0x16, 0x80, 0x80,
      0x00, 0x00, 0x03, 0x00, 0x80, 0x00, 0x00, 0x0f, 0x04, 0x00,
    ];
    expect(hevcCodecOf(new Uint8Array([0, 0, 0, 1, ...sps]))).toBe("hev1.4.10.L60.9F.A8");
  });
});

describe("AV1 low-overhead OBU support", () => {
  const av1Obu = (type: number, payload: number[]) => [(type << 3) | 2, payload.length, ...payload];
  const libaomSequence = [
    // Temporal delimiter, then the first sequence header emitted by libaom
    // for 8-bit 4:2:0 Main video.
    0x12, 0x00, 0x0a, 0x0a, 0x00, 0x00, 0x00, 0x02, 0xaf, 0xff, 0x9b, 0x5f, 0x20, 0x08,
  ];

  test("detects sequence-header key temporal units", () => {
    expect(containsIdr(new Uint8Array([...libaomSequence, ...av1Obu(6, [0x14])]), "av1")).toBe(
      true,
    );
    expect(containsIdr(new Uint8Array(av1Obu(6, [0x14])), "av1")).toBe(false);
    expect(containsIdr(new Uint8Array([...libaomSequence, ...av1Obu(6, [0x50])]), "av1")).toBe(
      false,
    );
  });

  test("derives the codec string from a real libaom sequence header", () => {
    expect(av1CodecOf(new Uint8Array(libaomSequence))).toBe("av01.0.00M.08");
  });

  test("derives the level from the first operating point", () => {
    const bits: number[] = [];
    const push = (value: number, width: number) => {
      for (let bit = width - 1; bit >= 0; bit--) bits.push((value >> bit) & 1);
    };
    push(0, 3); // profile 0
    push(0, 1); // still_picture
    push(0, 1); // reduced_still_picture_header
    push(0, 1); // timing_info_present
    push(0, 1); // initial_display_delay_present
    push(0, 5); // one operating point
    push(0, 12); // operating_point_idc[0]
    push(8, 5); // seq_level_idx[0]
    const payload: number[] = [];
    for (let i = 0; i < bits.length; i += 8) {
      payload.push(
        [...bits.slice(i, i + 8), ...Array(8 - Math.min(8, bits.length - i)).fill(0)].reduce(
          (byte, bit) => (byte << 1) | bit,
          0,
        ),
      );
    }
    expect(av1CodecOf(new Uint8Array(av1Obu(1, payload)))).toBe("av01.0.08M.08");
  });
});
