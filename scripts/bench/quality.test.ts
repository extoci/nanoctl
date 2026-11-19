import { expect, test } from "bun:test";
import { qualityScores, analyzeQuality, pcapPayloadBytes } from "./quality";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
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
