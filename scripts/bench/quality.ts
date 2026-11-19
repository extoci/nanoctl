import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { distribution } from "./metrics";

export function qualityScores(log: string) {
  const ssim = Number(log.match(/SSIM[^\n]*All:([\d.]+)/)?.[1]);
  const value = log.match(/PSNR[^\n]*average:([\d.]+|inf)/)?.[1];
  const psnr = value === "inf" ? null : Number(value);
  if (!Number.isFinite(ssim) || ssim < 0 || ssim > 1 || (value !== "inf" && !Number.isFinite(psnr)))
    throw new Error(`Invalid quality metrics: ${log.slice(-1000)}`);
  return { ssim, psnr, lossless: value === "inf" };
}

export function analyzeQuality(prefix: string, raw: string) {
  const pairs = raw
    .trim()
    .split("\n")
    .filter((line) => line.startsWith("P "))
    .map((line) => {
      const [, id, scale] = line.split(" ").map(Number);
      if (!Number.isInteger(id) || !Number.isFinite(scale) || scale <= 0)
        throw new Error("Invalid frame pair");
      const source = `${prefix}-${id}-source.ppm`,
        client = `${prefix}-${id}-client.ppm`;
      const filter =
        "[0:v]format=gray,split[a0][a1];[1:v]scale=1120:360:flags=lanczos,format=gray,split[b0][b1];[a0][b0]ssim[s];[a1][b1]psnr[p]";
      const result = Bun.spawnSync([
        "ffmpeg",
        "-hide_banner",
        "-threads",
        "1",
        "-filter_complex_threads",
        "1",
        "-i",
        source,
        "-i",
        client,
        "-filter_complex",
        filter,
        "-map",
        "[s]",
        "-map",
        "[p]",
        "-f",
        "null",
        "-",
      ]);
      if (result.exitCode !== 0) throw new Error(result.stderr.toString());
      return { id, scale, source, client, ...qualityScores(result.stderr.toString()) };
    });
  if (pairs.length < 10) throw new Error(`Only ${pairs.length} matched quality frames (need >=10)`);
  const first = pairs[Math.floor(pairs.length / 2)];
  for (const side of ["source", "client"] as const)
    execFileSync("ffmpeg", [
      "-hide_banner",
      "-loglevel",
      "error",
      "-threads",
      "1",
      "-i",
      first[side],
      "-vf",
      "scale=1120:360:flags=lanczos,crop=640:180:0:0",
      "-frames:v",
      "1",
      `${prefix}-text-${side}.png`,
    ]);
  return {
    pairs,
    ssim: distribution(pairs.map((p) => p.ssim)),
    psnrY: distribution(pairs.flatMap((p) => (p.psnr === null ? [] : [p.psnr]))),
    losslessPairs: pairs.filter((p) => p.lossless).length,
    convention:
      "FFmpeg gray luma, 8x8 SSIM and peak=255 PSNR; markers/chrome excluded; independently captured native 1:1 XShm ROIs",
  };
}

export function pcapPayloadBytes(path: string, sourcePort: number) {
  const bytes = readFileSync(path);
  const magic = bytes.readUInt32LE(0);
  if (magic !== 0xa1b2c3d4 || bytes.readUInt32LE(20) !== 1)
    throw new Error("Expected little-endian Ethernet pcap");
  let total = 0,
    packets = 0;
  for (let offset = 24; offset < bytes.length;) {
    if (offset + 16 > bytes.length) throw new Error("Truncated pcap header");
    const length = bytes.readUInt32LE(offset + 8),
      start = offset + 16;
    if (start + length > bytes.length) throw new Error("Truncated pcap packet");
    if (length >= 42 && bytes.readUInt16BE(start + 12) === 0x0800) {
      const ip = start + 14,
        header = (bytes[ip] & 15) * 4;
      if (
        header >= 20 &&
        bytes[ip + 9] === 17 &&
        ip + header + 8 <= start + length &&
        bytes.readUInt16BE(ip + header) === sourcePort
      ) {
        const udpLength = bytes.readUInt16BE(ip + header + 4);
        if (udpLength < 8) throw new Error("Invalid UDP length");
        total += udpLength - 8;
        packets++;
      }
    }
    offset = start + length;
  }
  return { bytes: total, packets };
}
