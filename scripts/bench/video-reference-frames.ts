/** Measures the zero-reordering-latency compression effect of multiple H.264 references. */
import { execFileSync, spawnSync } from "node:child_process";
import { mkdtempSync, rmSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const DURATION_SECONDS = 8;
const SCROLL_SPEED = "0.002083"; // 1.5 px/frame at 720p60.

type Source = {
  suffix: string;
  input: string[];
  encodeFilter?: string;
  referenceFilter?: string;
};

const encode = (directory: string, refs: number, source: Source): string => {
  const output = join(directory, `ref${refs}${source.suffix}.h264`);
  execFileSync(
    "ffmpeg",
    [
      "-hide_banner", "-loglevel", "error",
      ...source.input,
      ...(source.encodeFilter ? ["-vf", source.encodeFilter] : []),
      "-t", String(DURATION_SECONDS),
      "-an", "-c:v", "libx264", "-preset", "fast", "-tune", "zerolatency",
      "-bf", "0", "-pix_fmt", "yuv420p", "-crf", "18",
      "-maxrate", "24000k", "-bufsize", "24000k", "-profile:v", "high",
      "-x264-params",
      `keyint=2147483647:scenecut=0:me=umh:subme=6:threads=2:sliced-threads=1:ref=${refs}:trellis=0`,
      "-f", "h264", "-y", output,
    ],
    { stdio: ["ignore", "ignore", "inherit"] },
  );
  return output;
};

const ssim = (encoded: string, source: Source): number => {
  const filter = source.referenceFilter
    ? `[1:v]${source.referenceFilter}[reference];[0:v][reference]ssim`
    : "[0:v][1:v]ssim";
  const process = spawnSync(
    "ffmpeg",
    [
      "-hide_banner", "-loglevel", "info", "-i", encoded, ...source.input,
      "-filter_complex", filter, "-f", "null", "-",
    ],
    { encoding: "utf8", stdio: ["ignore", "ignore", "pipe"] },
  );
  const match = /All:([0-9.]+)/.exec(process.stderr ?? "");
  if (!match) throw new Error("ffmpeg did not report SSIM");
  return Number(match[1]);
};

const measure = (directory: string, refs: number, source: Source) => {
  const output = encode(directory, refs, source);
  return { bytes: statSync(output).size, ssim: ssim(output, source) };
};

const directory = mkdtempSync(join(tmpdir(), "nanoctl-video-refs-"));
try {
  const scrollImage = join(directory, "scroll-source.png");
  execFileSync(
    "ffmpeg",
    [
      "-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i",
      "testsrc=size=1280x720:rate=60", "-frames:v", "1", "-y", scrollImage,
    ],
    { stdio: ["ignore", "ignore", "inherit"] },
  );
  const sources: Source[] = [
    {
      suffix: "-stress",
      input: ["-f", "lavfi", "-i", "testsrc2=size=1280x720:rate=60:duration=8"],
    },
    {
      suffix: "-scroll",
      input: ["-loop", "1", "-framerate", "60", "-t", String(DURATION_SECONDS), "-i", scrollImage],
      encodeFilter: `scroll=vertical=${SCROLL_SPEED}`,
      referenceFilter: `scroll=vertical=${SCROLL_SPEED},fps=60`,
    },
  ];
  for (const source of sources) {
    const one = measure(directory, 1, source);
    const four = measure(directory, 4, source);
    console.log(
      `${source.suffix.slice(1)}: ref1 ${one.bytes} bytes @ SSIM ${one.ssim.toFixed(6)} -> ` +
        `ref4 ${four.bytes} bytes @ SSIM ${four.ssim.toFixed(6)} ` +
        `(${((1 - four.bytes / one.bytes) * 100).toFixed(1)}% fewer bytes, ` +
        `${(four.ssim - one.ssim).toFixed(6)} SSIM)`,
    );
    if (four.bytes >= one.bytes || four.ssim + 0.0002 < one.ssim) process.exitCode = 1;
  }
} finally {
  rmSync(directory, { recursive: true, force: true });
}
