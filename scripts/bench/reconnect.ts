import { chromium } from "@playwright/test";
import { existsSync, readFileSync, readdirSync } from "node:fs";
import { writeFile } from "node:fs/promises";
import { markerBits } from "../../tests/browser/latency-marker";

const [pidText, output] = process.argv.slice(2);
const pid = Number(pidText);
const cycles = Number(process.env.CYCLES ?? 20);
const motionMs = Number(process.env.MOTION_MS ?? 10000);
const settleMs = Number(process.env.SETTLE_MS ?? 3000);
if (
  !output ||
  !Number.isInteger(pid) ||
  pid < 1 ||
  !Number.isInteger(cycles) ||
  cycles < 1 ||
  !Number.isFinite(motionMs) ||
  motionMs < 1000 ||
  !Number.isFinite(settleMs) ||
  settleMs < 0
)
  throw new Error(
    "Usage: bun scripts/bench/reconnect.ts AGENT_PID OUTPUT_JSON; optional CYCLES, MOTION_MS, SETTLE_MS, VIEWER_URL",
  );
if (existsSync(output)) throw new Error(`Refusing to overwrite ${output}`);
function sample() {
  const status = readFileSync(`/proc/${pid}/status`, "utf8");
  const value = (name: string) =>
    Number(new RegExp(`^${name}:\\s+(\\d+)`, "m").exec(status)?.[1] ?? NaN);
  return {
    at: new Date().toISOString(),
    rssMiB: value("VmRSS") / 1024,
    threads: value("Threads"),
    fds: readdirSync(`/proc/${pid}/fd`).length,
  };
}
const rows: object[] = [];
const baseline = sample();
const source = await chromium.launch({
  headless: false,
  env: { ...process.env, DISPLAY: process.env.HOST_DISPLAY ?? ":97" },
  args: ["--kiosk", "--no-first-run", "--disable-dev-shm-usage"],
});
let client: Awaited<ReturnType<typeof chromium.launch>> | undefined;
try {
  client = await chromium.launch({
    headless: false,
    env: { ...process.env, DISPLAY: process.env.CLIENT_DISPLAY ?? ":94" },
    args: ["--kiosk", "--no-first-run", "--disable-dev-shm-usage"],
  });
  const host = await source.newPage({ viewport: null });
  await host.setContent(
    '<body style="margin:0;background:black"><canvas width="1280" height="720"></canvas></body>',
  );
  await host.evaluate((encode) => {
    const bits = new Function(`return (${encode})`)() as typeof markerBits;
    const canvas = document.querySelector("canvas")!;
    const ctx = canvas.getContext("2d")!;
    let sequence = 0;
    const draw = () => {
      ctx.fillStyle = "#151515";
      ctx.fillRect(0, 0, 1280, 720);
      ctx.fillStyle = "cyan";
      ctx.fillRect(0, 100, 80, 100);
      ctx.fillStyle = "magenta";
      ctx.fillRect(800, 100, 80, 100);
      bits(sequence, 0).forEach((bit, i) => {
        ctx.fillStyle = bit ? "white" : "black";
        ctx.fillRect((i + 4) * 20, 100, 20, 100);
      });
      ctx.fillStyle = "white";
      ctx.font = "36px sans-serif";
      ctx.fillText(`frame ${sequence}`, 80, 300);
      ctx.fillStyle = "#38bdf8";
      ctx.fillRect((sequence++ * 7) % 1120, 400, 160, 160);
      requestAnimationFrame(draw);
    };
    requestAnimationFrame(draw);
  }, markerBits.toString());
  const save = () =>
    writeFile(output, JSON.stringify({ pid, cycles, motionMs, settleMs, baseline, rows }, null, 2));
  await save();
  for (let cycle = 1; cycle <= cycles; cycle++) {
    const context = await client.newContext({ viewport: null });
    try {
      const viewer = await context.newPage();
      await viewer.goto(process.env.VIEWER_URL ?? "http://127.0.0.1:7338");
      await viewer.waitForFunction(() => {
        const video = document.querySelector("video");
        return video && video.videoWidth === 1280 && video.readyState >= 2;
      });
      await viewer.waitForTimeout(motionMs);
      const frames = await viewer.evaluate(
        () => document.querySelector("video")!.getVideoPlaybackQuality().totalVideoFrames,
      );
      if (frames < 30) throw new Error(`Cycle ${cycle}: only ${frames} presented video frames`);
      const connected = sample();
      await context.close();
      await new Promise((resolve) => setTimeout(resolve, settleMs));
      const disconnected = sample();
      rows.push({ cycle, frames, connected, disconnected });
      await save();
      console.log(
        `${cycle}: RSS ${disconnected.rssMiB.toFixed(2)} MiB; threads ${disconnected.threads}; fds ${disconnected.fds}; frames ${frames}`,
      );
    } finally {
      await context.close();
    }
  }
} finally {
  await client?.close();
  await source.close();
}
