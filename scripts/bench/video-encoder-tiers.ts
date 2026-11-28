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
};

const encode = (directory: string, tier: Tier): string => {
  const output = join(directory, `${tier.name}.h264`);
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
      `keyint=2147483647:scenecut=0:me=${tier.me}:subme=${tier.subme}`,
      "-f",
      "h264",
      "-y",
      output,
    ],
    { stdio: ["ignore", "ignore", "inherit"] },
  );
  return output;
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
  });
  const tier3 = encode(directory, {
    name: "tier3-high-umh",
    preset: "fast",
    profile: "high",
    me: "umh",
    subme: 6,
  });
  const beforeBytes = statSync(tier0).size;
  const afterBytes = statSync(tier3).size;
  const beforeSsim = ssim(tier0);
  const afterSsim = ssim(tier3);
  const saved = (1 - afterBytes / beforeBytes) * 100;
  console.log(
    `testsrc2 motion: ${beforeBytes} bytes @ SSIM ${beforeSsim.toFixed(6)} -> ` +
      `${afterBytes} bytes @ SSIM ${afterSsim.toFixed(6)} ` +
      `(${saved.toFixed(1)}% fewer bytes, ${(afterSsim - beforeSsim).toFixed(6)} SSIM)`,
  );
  if (afterBytes >= beforeBytes || afterSsim + 0.0002 < beforeSsim) process.exitCode = 1;
} finally {
  rmSync(directory, { recursive: true, force: true });
}
