/** Measures periodic-IDR cost and the exact master-to-experimental scrolling frontier. */
import { execFileSync, spawnSync } from "node:child_process";
import { mkdtempSync, rmSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const FPS = 60;
const DURATION_SECONDS = 25;
const SCROLL_SPEED = String(5 / 720); // 5 px/frame at 720p60.

const encode = (
  directory: string,
  image: string,
  name: string,
  periodic: boolean,
  subme: number,
  threads: number,
  refs: number,
): string => {
  const output = join(directory, `${name}.h264`);
  execFileSync(
    "ffmpeg",
    [
      "-hide_banner",
      "-loglevel",
      "error",
      "-loop",
      "1",
      "-framerate",
      String(FPS),
      "-t",
      String(DURATION_SECONDS),
      "-i",
      image,
      "-vf",
      `scroll=vertical=${SCROLL_SPEED}`,
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
      "18",
      "-maxrate",
      "24000k",
      "-bufsize",
      "24000k",
      "-profile:v",
      "high",
      "-x264-params",
      `keyint=${periodic ? 20 * FPS : 2147483647}:scenecut=0:me=umh:subme=${subme}:` +
        `threads=${threads}:sliced-threads=1:ref=${refs}:trellis=0`,
      "-f",
      "h264",
      "-y",
      output,
    ],
    { stdio: ["ignore", "ignore", "inherit"] },
  );
  return output;
};

const ssim = (encoded: string, image: string): number => {
  const process = spawnSync(
    "ffmpeg",
    [
      "-hide_banner",
      "-loglevel",
      "info",
      "-i",
      encoded,
      "-loop",
      "1",
      "-framerate",
      String(FPS),
      "-t",
      String(DURATION_SECONDS),
      "-i",
      image,
      "-filter_complex",
      `[1:v]scroll=vertical=${SCROLL_SPEED},fps=${FPS}[reference];[0:v][reference]ssim`,
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

const directory = mkdtempSync(join(tmpdir(), "nanoctl-periodic-idr-"));
try {
  const image = join(directory, "scroll-source.png");
  execFileSync(
    "ffmpeg",
    [
      "-hide_banner",
      "-loglevel",
      "error",
      "-f",
      "lavfi",
      "-i",
      "testsrc=size=1280x720:rate=60",
      "-frames:v",
      "1",
      "-y",
      image,
    ],
    { stdio: ["ignore", "ignore", "inherit"] },
  );
  const measure = (
    name: string,
    periodic: boolean,
    subme: number,
    threads: number,
    refs: number,
  ) => {
    const output = encode(directory, image, name, periodic, subme, threads, refs);
    return { bytes: statSync(output).size, ssim: ssim(output, image) };
  };
  const master = measure("master-periodic", true, 6, 2, 1);
  const before = measure("final-periodic", true, 9, 1, 4);
  const after = measure("final-repair-only", false, 9, 1, 4);
  const saved = (1 - after.bytes / before.bytes) * 100;
  console.log(
    `25s 5px/frame scrolling screen: periodic IDR ${before.bytes} bytes @ SSIM ${before.ssim.toFixed(6)} -> ` +
      `repair-only ${after.bytes} bytes @ SSIM ${after.ssim.toFixed(6)} ` +
      `(${saved.toFixed(1)}% fewer bytes, ${(after.ssim - before.ssim).toFixed(6)} SSIM)`,
  );
  console.log(
    `master tier3/ref1 periodic ${master.bytes} bytes @ SSIM ${master.ssim.toFixed(6)} -> ` +
      `experimental repair-only ${after.bytes} bytes @ SSIM ${after.ssim.toFixed(6)} ` +
      `(${(master.bytes / after.bytes).toFixed(2)}x lower, ${(after.ssim - master.ssim).toFixed(6)} SSIM)`,
  );
  if (after.bytes >= before.bytes || after.ssim + 0.0002 < before.ssim) process.exitCode = 1;
  if (master.bytes / after.bytes < 10 || after.ssim + 0.0002 < master.ssim) process.exitCode = 1;
} finally {
  rmSync(directory, { recursive: true, force: true });
}
