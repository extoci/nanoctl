/**
 * Exact zero-reorder codec frontier on moving 3D gameplay footage.
 *
 * Unlike the scrolling-screen benches, this uses a pinned copy of the public
 * x264-benchmark 3DMark Skydiver capture. It is deliberately measurement-only:
 * the numbers are the verifier output for both compression wins and regressions.
 */
import { execFileSync, spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const SOURCE_URL =
  "https://raw.githubusercontent.com/cmoore1776/x264-benchmark/master/skydiver_10sec.mp4";
const SOURCE_SHA256 = "5717c84e5f278091b4485e112963c8d6fa11db5a5c86791a2e90f0c443d3fb0c";
const DURATION_SECONDS = 5;
const WIDTH = 960;
const HEIGHT = 720;
const FPS = 60;

type Quality = { ssim: number; psnr: number; vmaf?: number };
type Result = Quality & { bytes: number; seconds: number };

const run = (command: string, args: string[]) => {
  execFileSync(command, args, { stdio: ["ignore", "ignore", "inherit"] });
};

const metric = (encoded: string, reference: string, name: "ssim" | "psnr" | "libvmaf") => {
  const process = spawnSync(
    "ffmpeg",
    [
      "-hide_banner",
      "-loglevel",
      "info",
      "-i",
      encoded,
      "-i",
      reference,
      "-t",
      String(DURATION_SECONDS),
      "-filter_complex",
      name,
      "-f",
      "null",
      "-",
    ],
    { encoding: "utf8", stdio: ["ignore", "ignore", "pipe"] },
  );
  const stderr = process.stderr ?? "";
  const pattern =
    name === "ssim"
      ? /All:([0-9.]+)/
      : name === "psnr"
        ? /average:([0-9.]+)/
        : /VMAF score: ([0-9.]+)/;
  const match = pattern.exec(stderr);
  return match ? Number(match[1]) : undefined;
};

const quality = (encoded: string, reference: string): Quality => {
  const ssim = metric(encoded, reference, "ssim");
  const psnr = metric(encoded, reference, "psnr");
  if (ssim === undefined || psnr === undefined) throw new Error("ffmpeg did not report SSIM/PSNR");
  return { ssim, psnr, vmaf: metric(encoded, reference, "libvmaf") };
};

const encode = (
  directory: string,
  reference: string,
  name: string,
  crf: number,
  subme: number,
  threads: number,
  refs: number,
): Result => {
  const output = join(directory, `${name}.h264`);
  const started = performance.now();
  run("ffmpeg", [
    "-hide_banner",
    "-loglevel",
    "error",
    "-i",
    reference,
    "-t",
    String(DURATION_SECONDS),
    "-an",
    "-c:v",
    "libx264",
    "-preset",
    "fast",
    "-tune",
    "zerolatency",
    "-bf",
    "0",
    "-pix_fmt",
    "yuv420p",
    "-crf",
    String(crf),
    "-maxrate",
    "24000k",
    "-bufsize",
    "24000k",
    "-profile:v",
    "high",
    "-x264-params",
    `keyint=2147483647:scenecut=0:me=umh:subme=${subme}:threads=${threads}:sliced-threads=1:ref=${refs}:trellis=0`,
    "-f",
    "h264",
    "-y",
    output,
  ]);
  const seconds = (performance.now() - started) / 1000;
  return { bytes: statSync(output).size, seconds, ...quality(output, reference) };
};

const fmtQuality = (result: Result) =>
  `SSIM ${result.ssim.toFixed(6)}, PSNR ${result.psnr.toFixed(3)}` +
  (result.vmaf === undefined ? "" : `, VMAF ${result.vmaf.toFixed(2)}`);

const cachedSource = join(tmpdir(), `nanoctl-skydiver-${SOURCE_SHA256.slice(0, 16)}.mp4`);
if (!existsSync(cachedSource)) {
  const response = await fetch(SOURCE_URL);
  if (!response.ok) throw new Error(`gameplay source download failed: HTTP ${response.status}`);
  writeFileSync(cachedSource, Buffer.from(await response.arrayBuffer()));
}
const digest = createHash("sha256").update(readFileSync(cachedSource)).digest("hex");
if (digest !== SOURCE_SHA256) throw new Error(`gameplay source hash mismatch: ${digest}`);

const directory = mkdtempSync(join(tmpdir(), "nanoctl-gameplay-bench-"));
try {
  const reference = join(directory, "gameplay-960x720-60.y4m");
  run("ffmpeg", [
    "-hide_banner",
    "-loglevel",
    "error",
    "-i",
    cachedSource,
    "-vf",
    `crop=1440:1080:240:0,scale=${WIDTH}:${HEIGHT},fps=${FPS}`,
    "-t",
    String(DURATION_SECONDS),
    "-an",
    "-pix_fmt",
    "yuv420p",
    "-f",
    "yuv4mpegpipe",
    "-y",
    reference,
  ]);

  const master = encode(directory, reference, "master-tier3-ref1", 18, 6, 2, 1);
  const finalTier3 = encode(directory, reference, "experimental-tier3-ref4", 18, 6, 2, 4);
  const finalTier4 = encode(directory, reference, "experimental-tier4-ref4", 18, 9, 1, 4);
  const collapse = encode(directory, reference, "master-tier3-crf36", 36, 6, 2, 1);
  const ratio = master.bytes / finalTier4.bytes;
  const targetBytes = master.bytes / 10;

  console.log(
    `5s moving 3D gameplay ${WIDTH}x${HEIGHT}@${FPS}: master tier3/ref1 ${master.bytes} bytes ` +
      `(${fmtQuality(master)}, encode wall ${master.seconds.toFixed(2)}s)`,
  );
  console.log(
    `experimental tier3/ref4: ${finalTier3.bytes} bytes (${fmtQuality(finalTier3)}, encode wall ` +
      `${finalTier3.seconds.toFixed(2)}s); bytes ${(finalTier3.bytes / master.bytes).toFixed(3)}x master`,
  );
  console.log(
    `experimental tier4/ref4: ${finalTier4.bytes} bytes (${fmtQuality(finalTier4)}, encode wall ` +
      `${finalTier4.seconds.toFixed(2)}s); master/tier4 ratio ${ratio.toFixed(2)}x`,
  );
  console.log(
    `10x-equivalent target is <= ${Math.round(targetBytes)} bytes at master quality; ` +
      `CRF36 collapse point ${collapse.bytes} bytes (${(master.bytes / collapse.bytes).toFixed(2)}x lower) ` +
      `but quality falls to ${fmtQuality(collapse)}`,
  );
} finally {
  rmSync(directory, { recursive: true, force: true });
}
