/** Measures the x264 compression-tier frontier used by network-aware adaptation. */
import { execFileSync, spawnSync } from "node:child_process";
import { mkdtempSync, rmSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const SOURCE = "testsrc2=size=1280x720:rate=60:duration=8";

type Tier = {
  name: string;
  preset: string;
  profile: string;
  me: string;
  subme: number;
  threads: number;
  refs: number;
};

const encode = (directory: string, tier: Tier): { output: string; seconds: number } => {
  const output = join(directory, `${tier.name}.h264`);
  const started = performance.now();
  execFileSync(
    "ffmpeg",
    [
      "-hide_banner",
      "-loglevel",
      "error",
      "-f",
      "lavfi",
      "-i",
      SOURCE,
      "-an",
      "-c:v",
      "libx264",
      "-preset",
      tier.preset,
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
      tier.profile,
      "-x264-params",
      `keyint=2147483647:scenecut=0:me=${tier.me}:subme=${tier.subme}:` +
        `threads=${tier.threads}:sliced-threads=1:ref=${tier.refs}`,
      "-f",
      "h264",
      "-y",
      output,
    ],
    { stdio: ["ignore", "ignore", "inherit"] },
  );
  return { output, seconds: (performance.now() - started) / 1000 };
};

const ssim = (encoded: string): number => {
  const process = spawnSync(
    "ffmpeg",
    [
      "-hide_banner",
      "-loglevel",
      "info",
      "-i",
      encoded,
      "-f",
      "lavfi",
      "-i",
      SOURCE,
      "-filter_complex",
      "[0:v][1:v]ssim",
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

const directory = mkdtempSync(join(tmpdir(), "nanoctl-video-tiers-"));
try {
  const tier0 = encode(directory, {
    name: "tier0-baseline-hex",
    preset: "veryfast",
    profile: "baseline",
    me: "hex",
    subme: 2,
    threads: 0,
    refs: 1,
  });
  const tier3 = encode(directory, {
    name: "tier3-high-umh",
    preset: "fast",
    profile: "high",
    me: "umh",
    subme: 6,
    threads: 2,
    refs: 2,
  });
  const tier4 = encode(directory, {
    name: "tier4-single-slice-ref4",
    preset: "fast",
    profile: "high",
    me: "umh",
    subme: 6,
    threads: 1,
    refs: 4,
  });
  const rows = [tier0, tier3, tier4].map((tier) => ({
    name: tier.output.split(/[\\/]/).pop()!,
    bytes: statSync(tier.output).size,
    ssim: ssim(tier.output),
    seconds: tier.seconds,
  }));
  const [before, current, next] = rows;
  for (const row of rows) {
    console.log(
      `${row.name}: ${row.bytes} bytes @ SSIM ${row.ssim.toFixed(6)}, encode ${row.seconds.toFixed(2)}s`,
    );
  }
  const totalSaved = (1 - next.bytes / before.bytes) * 100;
  const incrementalSaved = (1 - next.bytes / current.bytes) * 100;
  console.log(
    `tier4 vs tier3: ${incrementalSaved.toFixed(1)}% fewer bytes, ` +
      `${(next.ssim - current.ssim).toFixed(6)} SSIM; tier4 vs tier0: ${totalSaved.toFixed(1)}% fewer bytes`,
  );
  if (current.bytes >= before.bytes || current.ssim + 0.0002 < before.ssim) process.exitCode = 1;
  if (next.bytes >= current.bytes || next.ssim + 0.0002 < current.ssim) process.exitCode = 1;
} finally {
  rmSync(directory, { recursive: true, force: true });
}
