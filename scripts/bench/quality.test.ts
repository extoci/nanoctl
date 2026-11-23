import { expect, test } from "bun:test";
import {
  qualityScores,
  analyzeQuality,
  pcapPayloadBytes,
  receivedVideoRates,
  idleVideoBursts,
} from "./quality";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

test("video bitrate excludes candidate-pair and audio transport counters", () => {
  const video = { type: "inbound-rtp", kind: "video", ssrc: 123 };
  expect(
    receivedVideoRates([
      { ...video, bytesReceived: 1000, timestamp: 100 },
      { type: "candidate-pair", bytesReceived: 99999999, timestamp: 100 },
      { ...video, kind: "audio", bytesReceived: 99999999, timestamp: 100 },
      { ...video, bytesReceived: 126000, timestamp: 1100 },
    ]),
  ).toEqual([{ ssrc: 123, mbps: 1 }]);
  expect(receivedVideoRates([])).toEqual([]);
  expect(receivedVideoRates([null, { ...video, bytesReceived: 10, timestamp: 1 }])).toEqual([]);
});

test("idle burst windows separate the refinement from interactive input and quiet", () => {
  const rows = [0, 100, 200, 300, 500, 600].map((at, i) => ({
    at,
    reports: [
      {
        type: "inbound-rtp",
        kind: "video",
        ssrc: 1,
        bytesReceived: [100, 1100, 5100, 5100, 5100, 5100][i],
      },
      { type: "inbound-rtp", kind: "audio", ssrc: 2, bytesReceived: 99999999 },
    ],
  }));
  const result = idleVideoBursts(rows, [0]);
  expect(result.windows.interactive.bytes).toBe(1000);
  expect(result.windows.refinement.bytes).toBe(4000);
  expect(result.windows.quiet.bytes).toBe(0);
  expect(result.peakIntervalMbps.max).toBe(0.32);
  expect(idleVideoBursts([], []).intervals).toEqual([]);
});

test("idle burst analysis rejects invalid clocks and counter resets", () => {
  expect(() => idleVideoBursts([], [NaN])).toThrow("paint");
  expect(() => idleVideoBursts([{ at: NaN, reports: [] }], [])).toThrow("sample");
  expect(() =>
    idleVideoBursts(
      [
        { at: 1, reports: [] },
        { at: 1, reports: [] },
      ],
      [],
    ),
  ).toThrow("sample");
  const report = { type: "inbound-rtp", kind: "video", ssrc: 1, bytesReceived: 100 };
  expect(() =>
    idleVideoBursts(
      [
        { at: 0, reports: [report] },
        { at: 100, reports: [{ ...report, bytesReceived: 0 }] },
      ],
      [0],
    ),
  ).toThrow("monotonically");
});

test("video bitrate rejects invalid or reset counters", () => {
  const video = { type: "inbound-rtp", kind: "video", ssrc: 123, timestamp: 1, bytesReceived: 100 };
  expect(() => receivedVideoRates([{ ...video, bytesReceived: NaN }])).toThrow("Invalid");
  expect(() => receivedVideoRates([video, { ...video, timestamp: 2, bytesReceived: 1 }])).toThrow(
    "monotonically",
  );
  expect(() => receivedVideoRates([video, video])).toThrow("monotonically");
});
test("quality parser preserves finite scores and represents perfect PSNR explicitly", () => {
  expect(qualityScores("SSIM Y:0.99 All:0.998\nPSNR y:42 average:42.15 min:42")).toEqual({
    ssim: 0.998,
    psnr: 42.15,
    lossless: false,
  });
  expect(qualityScores("SSIM All:1.000\nPSNR average:inf")).toEqual({
    ssim: 1,
    psnr: null,
    lossless: true,
  });
});

test("pcap counter uses full UDP lengths from snaplen headers and rejects truncated files", () => {
  const root = mkdtempSync(join(tmpdir(), "nanoctl-pcap-")),
    path = join(root, "test.pcap");
  try {
    const header = Buffer.alloc(24);
    header.writeUInt32LE(0xa1b2c3d4);
    header.writeUInt32LE(1, 20);
    writeFileSync(path, header);
    expect(pcapPayloadBytes(path, 49009)).toEqual({ bytes: 0, packets: 0 });
    const record = Buffer.alloc(16),
      packet = Buffer.alloc(60);
    record.writeUInt32LE(60, 8);
    packet.writeUInt16BE(0x0800, 12);
    packet[14] = 0x45;
    packet[23] = 17;
    packet.writeUInt16BE(49009, 34);
    packet.writeUInt16BE(1200, 38);
    writeFileSync(path, Buffer.concat([header, record, packet]));
    expect(pcapPayloadBytes(path, 49009)).toEqual({ bytes: 1192, packets: 1 });
    expect(pcapPayloadBytes(path, 49010)).toEqual({ bytes: 0, packets: 0 });
    writeFileSync(path, Buffer.concat([header, record]));
    expect(() => pcapPayloadBytes(path, 49009)).toThrow("Truncated");
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
test("quality parser rejects missing or impossible metrics and empty matching sets", () => {
  for (const log of ["", "SSIM All:1.5 PSNR average:42", "SSIM All:0.9"])
    expect(() => qualityScores(log)).toThrow();
  expect(() => analyzeQuality("unused", "")).toThrow("Only 0 matched");
});
