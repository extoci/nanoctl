/**
 * Models the first four seconds of x264 adaptation on trivially easy scrolling
 * screen content. Packet sizes come from exact ffmpeg/libx264 encodes; the
 * frame schedule mirrors AdaptPolicy (one 30-frame trivial-content window, then either
 * the old 90-frame tier2 window or the direct top-tier jump).
 */
import { execFileSync } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const FPS = 60;
const SCROLL_SPEED = "0.002083";

type Tier = {
  name: string;
  preset: string;
  profile: string;
  me: string;
  subme: number;
  threads: number;
  refs: number;
};

const tiers: Tier[] = [
  {
    name: "tier0",
    preset: "veryfast",
    profile: "baseline",
    me: "hex",
    subme: 2,
    threads: 0,
    refs: 1,
  },
  { name: "tier2", preset: "veryfast", profile: "high", me: "umh", subme: 2, threads: 2, refs: 1 },
  { name: "tier4", preset: "fast", profile: "high", me: "umh", subme: 6, threads: 1, refs: 4 },
];

type Packets = { first: number; delta: number };
const encode = (directory: string, image: string, tier: Tier): Packets => {
  const output = join(directory, `${tier.name}.h264`);
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
      "8",
      "-i",
      image,
      "-vf",
      `scroll=vertical=${SCROLL_SPEED}`,
      "-t",
      "8",
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
  const probe = execFileSync(
    "ffprobe",
    [
      "-v",
      "error",
      "-select_streams",
      "v:0",
      "-show_entries",
      "packet=size",
      "-of",
      "json",
      output,
    ],
    { encoding: "utf8" },
  );
  const sizes = (JSON.parse(probe) as { packets: { size: string }[] }).packets.map((packet) =>
    Number(packet.size),
  );
  return {
    first: sizes[0]!,
    delta: sizes.slice(1).reduce((sum, size) => sum + size, 0) / (sizes.length - 1),
  };
};

const cost = (packets: Packets, frames: number) => packets.first + (frames - 1) * packets.delta;
const directory = mkdtempSync(join(tmpdir(), "nanoctl-adaptive-startup-"));
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
  const measured = new Map(tiers.map((tier) => [tier.name, encode(directory, image, tier)]));
  const tier0 = measured.get("tier0")!;
  const tier2 = measured.get("tier2")!;
  const tier4 = measured.get("tier4")!;
  // Before: 1s tier0, 1.5s tier2, then 1.5s tier4 (plus one re-open IDR each).
  // After: 0.5s tier0, then 3.5s directly in tier4 (one fewer re-open IDR).
  const before = cost(tier0, 60) + cost(tier2, 90) + cost(tier4, 90);
  const after = cost(tier0, 30) + cost(tier4, 210);
  const kbps = (bytes: number) => ((bytes * 8) / 4 / 1000).toFixed(1);
  const saved = (1 - after / before) * 100;
  for (const [name, packets] of measured) {
    console.log(
      `${name}: IDR ${packets.first} bytes, mean delta ${packets.delta.toFixed(1)} bytes`,
    );
  }
  console.log(
    `first 4s trivial scrolling screen: ${kbps(before)} -> ${kbps(after)} kbps ` +
      `(${saved.toFixed(1)}% less)`,
  );
  if (saved < 20) process.exitCode = 1;
} finally {
  rmSync(directory, { recursive: true, force: true });
}
