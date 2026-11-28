/** Measures the x264 compression-tier frontier used by network-aware adaptation. */
import { execFileSync, spawnSync } from "node:child_process";
import { mkdtempSync, rmSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const STRESS_SOURCE = "testsrc2=size=1280x720:rate=60:duration=8";
const SCROLL_SPEED = String(5 / 720); // 5 px/frame at 720p60, typical smooth scrolling.
const DURATION_SECONDS = 8;

type Tier = {
  name: string;
  preset: string;
  profile: string;
  me: string;
  subme: number;
  threads: number;
  refs: number;
};

type Source = {
  suffix: string;
  input: string[];
  encodeFilter?: string;
  referenceFilter?: string;
};

const encode = (
  directory: string,
  tier: Tier,
  source: Source,
): { output: string; seconds: number } => {
  const output = join(directory, `${tier.name}${source.suffix}.h264`);
  const started = performance.now();
  execFileSync(
    "ffmpeg",
    [
      "-hide_banner",
      "-loglevel",
      "error",
      ...source.input,
      ...(source.encodeFilter ? ["-vf", source.encodeFilter] : []),
      "-t",
      String(DURATION_SECONDS),
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
        `threads=${tier.threads}:sliced-threads=1:ref=${tier.refs}:trellis=0`,
      "-f",
      "h264",
      "-y",
      output,
    ],
    { stdio: ["ignore", "ignore", "inherit"] },
  );
  return { output, seconds: (performance.now() - started) / 1000 };
};

const ssim = (encoded: string, source: Source): number => {
  const filter = source.referenceFilter
    ? `[1:v]${source.referenceFilter}[reference];[0:v][reference]ssim`
    : "[0:v][1:v]ssim";
  const process = spawnSync(
    "ffmpeg",
    [
      "-hide_banner",
      "-loglevel",
      "info",
      "-i",
      encoded,
      ...source.input,
      "-filter_complex",
      filter,
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

const measure = (directory: string, tier: Tier, source: Source) => {
  const encoded = encode(directory, tier, source);
  return {
    name: `${tier.name}${source.suffix}`,
    bytes: statSync(encoded.output).size,
    ssim: ssim(encoded.output, source),
    seconds: encoded.seconds,
  };
};

const directory = mkdtempSync(join(tmpdir(), "nanoctl-video-tiers-"));
try {
  const scrollImage = join(directory, "scroll-source.png");
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
      scrollImage,
    ],
    { stdio: ["ignore", "ignore", "inherit"] },
  );
  const stress: Source = {
    suffix: "-stress",
    input: ["-f", "lavfi", "-i", STRESS_SOURCE],
  };
  const scroll: Source = {
    suffix: "-scroll",
    input: ["-loop", "1", "-framerate", "60", "-t", String(DURATION_SECONDS), "-i", scrollImage],
    encodeFilter: `scroll=vertical=${SCROLL_SPEED}`,
    referenceFilter: `scroll=vertical=${SCROLL_SPEED},fps=60`,
  };
  const tier0: Tier = {
    name: "tier0-baseline-hex",
    preset: "veryfast",
    profile: "baseline",
    me: "hex",
    subme: 2,
    threads: 0,
    refs: 1,
  };
  const masterTier3: Tier = {
    name: "master-tier3-high-umh-ref1",
    preset: "fast",
    profile: "high",
    me: "umh",
    subme: 6,
    threads: 2,
    refs: 1,
  };
  const tier3: Tier = {
    name: "tier3-high-umh",
    preset: "fast",
    profile: "high",
    me: "umh",
    subme: 6,
    threads: 2,
    refs: 4,
  };
  const tier4: Tier = {
    name: "tier4-single-slice-ref4",
    preset: "fast",
    profile: "high",
    me: "umh",
    subme: 9,
    threads: 1,
    refs: 4,
  };
  const rows = [tier0, masterTier3, tier3, tier4].flatMap((tier) => [
    measure(directory, tier, stress),
    measure(directory, tier, scroll),
  ]);
  for (const row of rows) {
    console.log(
      `${row.name}: ${row.bytes} bytes @ SSIM ${row.ssim.toFixed(6)}, encode ${row.seconds.toFixed(2)}s`,
    );
  }
  const before = rows.find((row) => row.name === "tier0-baseline-hex-stress")!;
  const current = rows.find((row) => row.name === "tier3-high-umh-stress")!;
  const next = rows.find((row) => row.name === "tier4-single-slice-ref4-stress")!;
  const totalSaved = (1 - next.bytes / before.bytes) * 100;
  const incrementalSaved = (1 - next.bytes / current.bytes) * 100;
  console.log(
    `tier4 vs tier3 stress: ${incrementalSaved.toFixed(1)}% fewer bytes, ` +
      `${(next.ssim - current.ssim).toFixed(6)} SSIM; tier4 vs tier0: ${totalSaved.toFixed(1)}% fewer bytes`,
  );
  const masterScroll = rows.find((row) => row.name === "master-tier3-high-umh-ref1-scroll")!;
  const scroll3 = rows.find((row) => row.name === "tier3-high-umh-scroll")!;
  const scroll4 = rows.find((row) => row.name === "tier4-single-slice-ref4-scroll")!;
  const scroll0 = rows.find((row) => row.name === "tier0-baseline-hex-scroll")!;
  console.log(
    `tier4 vs master tier3 scrolling screen: ${((1 - scroll4.bytes / masterScroll.bytes) * 100).toFixed(1)}% fewer bytes ` +
      `(${(masterScroll.bytes / scroll4.bytes).toFixed(2)}x lower), ` +
      `${(scroll4.ssim - masterScroll.ssim).toFixed(6)} SSIM; tier4 vs tier3: ` +
      `${((1 - scroll4.bytes / scroll3.bytes) * 100).toFixed(1)}% fewer; tier4 vs tier0: ` +
      `${((1 - scroll4.bytes / scroll0.bytes) * 100).toFixed(1)}% fewer`,
  );
  if (current.bytes >= before.bytes || current.ssim + 0.0002 < before.ssim) process.exitCode = 1;
  if (next.bytes >= current.bytes || next.ssim + 0.0002 < current.ssim) process.exitCode = 1;
  if (scroll4.bytes >= masterScroll.bytes || scroll4.ssim + 0.0002 < masterScroll.ssim)
    process.exitCode = 1;
} finally {
  rmSync(directory, { recursive: true, force: true });
}
