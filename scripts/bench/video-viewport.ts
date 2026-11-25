/** Benchmark viewer-sized video encoding with proportional bitrate caps. */
import { execFileSync, spawnSync } from "node:child_process";
import { mkdtempSync, rmSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const SOURCE_WIDTH = 1920;
const SOURCE_HEIGHT = 1080;
const FULL_BITRATE_KBPS = 12_000;
const SECONDS = 5;
const FPS = 60;

const encode = (
  directory: string,
  name: string,
  width: number,
  height: number,
  bitrateKbps: number,
): number => {
  const output = join(directory, `${name}.mp4`);
  const filter =
    `testsrc2=size=${SOURCE_WIDTH}x${SOURCE_HEIGHT}:rate=${FPS}:duration=${SECONDS},noise=alls=10:allf=t+u` +
    (width === SOURCE_WIDTH && height === SOURCE_HEIGHT ? "" : `,scale=${width}:${height}`);
  execFileSync(
    "ffmpeg",
    [
      "-hide_banner",
      "-loglevel",
      "error",
      "-f",
      "lavfi",
      "-i",
      filter,
      "-an",
      "-c:v",
      "libx264",
      "-preset",
      "veryfast",
      "-tune",
      "zerolatency",
      "-bf",
      "0",
      "-pix_fmt",
      "yuv420p",
      "-b:v",
      `${bitrateKbps}k`,
      "-maxrate",
      `${bitrateKbps}k`,
      "-bufsize",
      `${Math.round(bitrateKbps / 2)}k`,
      "-x264-params",
      "keyint=1800:min-keyint=1800:scenecut=0",
      "-y",
      output,
    ],
    { stdio: ["ignore", "ignore", "inherit"] },
  );
  return statSync(output).size;
};

const ssimAtViewport = (directory: string, name: string, width: number, height: number): number => {
  const source = `testsrc2=size=${SOURCE_WIDTH}x${SOURCE_HEIGHT}:rate=${FPS}:duration=${SECONDS},noise=alls=10:allf=t+u`;
  const process = spawnSync(
    "ffmpeg",
    [
      "-hide_banner",
      "-loglevel",
      "info",
      "-i",
      join(directory, `${name}.mp4`),
      "-f",
      "lavfi",
      "-i",
      source,
      "-filter_complex",
      `[0:v]scale=${width}:${height}[a];[1:v]scale=${width}:${height}[b];[a][b]ssim`,
      "-f",
      "null",
      "-",
    ],
    { encoding: "utf8", stdio: ["ignore", "ignore", "pipe"] },
  );
  const match = /All:([0-9.]+)/.exec(process.stderr ?? "");
  if (!match) throw new Error("ffmpeg did not report SSIM");
  return Number(match[1]);
};

const directory = mkdtempSync(join(tmpdir(), "nanoctl-video-viewport-"));
try {
  const full = encode(directory, "full", SOURCE_WIDTH, SOURCE_HEIGHT, FULL_BITRATE_KBPS);
  console.log(
    `source ${SOURCE_WIDTH}x${SOURCE_HEIGHT} @ ${FULL_BITRATE_KBPS} kbps cap: ${full} bytes`,
  );
  for (const [width, height] of [
    [1280, 720],
    [960, 540],
  ] as const) {
    const pixelRatio = (width * height) / (SOURCE_WIDTH * SOURCE_HEIGHT);
    const beforeCap = Math.round(FULL_BITRATE_KBPS * Math.pow(pixelRatio, 0.5));
    const cap = Math.round(FULL_BITRATE_KBPS * Math.pow(pixelRatio, 0.8));
    const beforeBytes = encode(directory, `${width}x${height}-sqrt`, width, height, beforeCap);
    const bytes = encode(directory, `${width}x${height}`, width, height, cap);
    const saved = 1 - bytes / beforeBytes;
    const fullSsim = ssimAtViewport(directory, "full", width, height);
    const beforeSsim = ssimAtViewport(directory, `${width}x${height}-sqrt`, width, height);
    const viewportSsim = ssimAtViewport(directory, `${width}x${height}`, width, height);
    console.log(
      `${width}x${height} viewport @ ${beforeCap} -> ${cap} kbps cap: ${beforeBytes} -> ${bytes} bytes (${(saved * 100).toFixed(1)}% less, ${(100 * (1 - bytes / full)).toFixed(1)}% below full), SSIM at viewport ${viewportSsim.toFixed(4)} vs ${beforeSsim.toFixed(4)} before and ${fullSsim.toFixed(4)} downscaled`,
    );
  }
} finally {
  rmSync(directory, { recursive: true, force: true });
}
