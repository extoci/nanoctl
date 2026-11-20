import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { distribution } from "./metrics";

export function receivedVideoRates(samples: unknown[]) {
  const streams = new Map<number, { bytes: number; at: number }[]>();
  for (const value of samples) {
    if (!value || typeof value !== "object") continue;
    const row = value as Record<string, unknown>;
    if (row.type !== "inbound-rtp" || row.kind !== "video") continue;
    const { ssrc, bytesReceived: bytes, timestamp: at } = row;
    if (
      typeof ssrc !== "number" ||
      !Number.isFinite(ssrc) ||
      typeof bytes !== "number" ||
      !Number.isFinite(bytes) ||
      bytes < 0 ||
      typeof at !== "number" ||
      !Number.isFinite(at)
    )
      throw new Error("Invalid inbound video counters");
    const rows = streams.get(ssrc) ?? [];
    rows.push({ bytes, at });
    streams.set(ssrc, rows);
  }
  return [...streams].flatMap(([ssrc, rows]) => {
    rows.sort((a, b) => a.at - b.at);
    if (rows.length < 2) return [];
    const first = rows[0],
      last = rows.at(-1)!;
    if (last.at <= first.at || last.bytes < first.bytes)
      throw new Error("Inbound video counters did not advance monotonically");
    return [{ ssrc, mbps: ((last.bytes - first.bytes) * 8) / (last.at - first.at) / 1000 }];
  });
}

export function idleVideoBursts(samples: { at: number; reports: unknown[] }[], paints: number[]) {
  if (paints.some((at, i) => !Number.isFinite(at) || (i > 0 && at <= paints[i - 1])))
    throw new Error("Invalid source paint times");
  const windows = {
    interactive: { bytes: 0, seconds: 0, intervals: 0 },
    refinement: { bytes: 0, seconds: 0, intervals: 0 },
    quiet: { bytes: 0, seconds: 0, intervals: 0 },
  };
  const intervals = [];
  let paint = -1;
  for (let i = 0; i < samples.length; i++) {
    const current = samples[i],
      previous = samples[i - 1];
    if (!Number.isFinite(current.at) || (previous && current.at <= previous.at))
      throw new Error("Invalid receiver sample times");
    if (!previous) continue;
    const rates = receivedVideoRates(
      [previous, current].flatMap((s) =>
        s.reports.map((value) =>
          value && typeof value === "object" ? { ...value, timestamp: s.at } : value,
        ),
      ),
    );
    const middle = (previous.at + current.at) / 2;
    while (paint + 1 < paints.length && paints[paint + 1] <= middle) paint++;
    if (paint < 0 || current.at > (paints[paint + 1] ?? Infinity)) continue;
    const age = middle - paints[paint];
    const phase = age < 100 ? "interactive" : age < 400 ? "refinement" : age < 700 ? "quiet" : null;
    if (!phase) continue;
    const seconds = (current.at - previous.at) / 1000;
    const mbps = rates.reduce((sum, rate) => sum + rate.mbps, 0);
    const bytes = Math.round((mbps * seconds * 1e6) / 8);
    windows[phase].bytes += bytes;
    windows[phase].seconds += seconds;
    windows[phase].intervals++;
    intervals.push({ at: middle, age, phase, bytes, mbps });
  }
  return {
    windows: Object.fromEntries(
      Object.entries(windows).map(([phase, w]) => [
        phase,
        {
          ...w,
          mbps: w.seconds > 0 ? (w.bytes * 8) / w.seconds / 1e6 : 0,
        },
      ]),
    ),
    peakIntervalMbps: distribution(intervals.map((i) => i.mbps)),
    intervals,
    convention:
      "Video RTP bytes; approximately 100-ms receiver intervals classified by midpoint after source paint: 0–100 ms interactive, 100–400 ms refinement, 400–700 ms quiet; intervals crossing the next source paint excluded",
  };
}

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
