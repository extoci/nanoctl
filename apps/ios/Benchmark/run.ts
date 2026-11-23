import { chromium } from "@playwright/test";
import { mkdir, rm } from "node:fs/promises";
import { join, resolve } from "node:path";
import { markerBits, summarize } from "../../../tests/browser/latency-marker";
import { rtpMarkerArrivals } from "./rtp-trace";

const root = resolve(import.meta.dir, "../../..");
const [app, device, output] = process.argv.slice(2);
if (!app || !device || !output)
  throw new Error("Usage: bun apps/ios/Benchmark/run.ts APP_PATH SIMULATOR_ID OUTPUT_DIR");
const endpoint = "http://127.0.0.1:7337";
const resultsPath = resolve(output);
const bundle = "ai.nanoctl.viewer";
const dist = join(root, "apps/viewer/dist");

interface Frame {
  sequence: number;
  input: number;
  displayMs: number;
  receiveMs?: number;
  presentationMs?: number;
  processingMs?: number;
  stages?: {
    rtp: number;
    decoder?: { start: number; end?: number };
    mailbox: number;
    renderStart: number;
    submit: number;
    gpuEnd: number;
  };
}
interface Receiver {
  ssrc?: number;
  decoder?: string;
  powerEfficientDecoder?: boolean;
  counters: Record<string, number | undefined>;
}
interface Probe {
  frames: Frame[];
  inputTimes: number[];
  width: number;
  height: number;
  epochOffset?: number;
  receiverStart?: Receiver;
  receiverEnd?: Receiver;
}
type HostWindow = typeof window & { nanoctlMarker: { paintTimes: number[]; input: number } };

function timingBreakdown(frames: Frame[], markers: number[], report: Probe) {
  const samples = new Map<string, number[]>();
  const add = (name: string, start: number | undefined, end: number | undefined) => {
    if (start === undefined || end === undefined || !Number.isFinite(end - start)) return;
    const series = samples.get(name) ?? [];
    series.push(end - start);
    samples.set(name, series);
  };
  const toEpoch = (at?: number) =>
    at === undefined || report.epochOffset === undefined
      ? undefined
      : (at + report.epochOffset) * 1000;
  for (const { stages, displayMs, receiveMs, presentationMs, processingMs, sequence } of frames) {
    add("hostToReceive", markers[sequence], receiveMs);
    add("receiveToDisplay", receiveMs, displayMs);
    if (stages) {
      const decodeStart = toEpoch(stages.decoder?.start);
      const decoded = toEpoch(stages.decoder?.end);
      add("receiveToDecodeStart", receiveMs, decodeStart);
      add("decode", decodeStart, decoded);
      add("decodeToMailbox", decoded, toEpoch(stages.mailbox));
      add("mailboxWait", toEpoch(stages.mailbox), toEpoch(stages.renderStart));
      add("renderPreparation", toEpoch(stages.renderStart), toEpoch(stages.submit));
      add("submitToGPUCompletionCallback", toEpoch(stages.submit), toEpoch(stages.gpuEnd));
      add("gpuCompletionCallbackToDisplay", toEpoch(stages.gpuEnd), displayMs);
    } else {
      add("receiveToCompositor", receiveMs, presentationMs);
      add("compositorToDisplay", presentationMs, displayMs);
      add("processing", 0, processingMs);
    }
  }
  return Object.fromEntries(Array.from(samples, ([name, series]) => [name, summarize(series)]));
}

function receiverAverages(start?: Receiver, end?: Receiver) {
  if (!start || !end || start.ssrc !== end.ssrc) return undefined;
  const diff = (key: string) => (end.counters[key] ?? NaN) - (start.counters[key] ?? NaN);
  const avg = (seconds: string, count: string) => {
    const denominator = diff(count);
    const value = (1000 * diff(seconds)) / denominator;
    return denominator > 0 && Number.isFinite(value) ? value : undefined;
  };
  return {
    decoder: end.decoder,
    powerEfficientDecoder: end.powerEfficientDecoder,
    jitterBufferMs: avg("jitterBufferDelay", "jitterBufferEmittedCount"),
    jitterBufferTargetMs: avg("jitterBufferTargetDelay", "jitterBufferEmittedCount"),
    jitterBufferMinimumMs: avg("jitterBufferMinimumDelay", "jitterBufferEmittedCount"),
    decodeMs: avg("totalDecodeTime", "framesDecoded"),
    processingMs: avg("totalProcessingDelay", "framesDecoded"),
    framesDropped: diff("framesDropped"),
  };
}

async function command(args: string[], env?: NodeJS.ProcessEnv) {
  const child = Bun.spawn(args, { cwd: root, env, stdout: "pipe", stderr: "inherit" });
  const out = await new Response(child.stdout).text();
  if (await child.exited) throw new Error(`${args.join(" ")} failed`);
  return out.trim();
}

const sim = (...args: string[]) => command(["xcrun", "simctl", ...args]);
const ignore = (promise: Promise<unknown>) => promise.catch(() => {});
const wait = async <T>(probe: () => Promise<T | undefined>, seconds: number, label: string) => {
  const deadline = performance.now() + seconds * 1000;
  while (performance.now() < deadline) {
    const result = await probe();
    if (result !== undefined) return result;
    await Bun.sleep(200);
  }
  throw new Error(`Timed out: ${label}`);
};

let safariReport: Probe | undefined;
const host = Bun.serve({
  hostname: "127.0.0.1",
  port: 7338,
  async fetch(request) {
    if (new URL(request.url).pathname === "/results" && request.method === "POST") {
      const body = await request.formData();
      safariReport = JSON.parse(String(body.get("report"))) as Probe;
      return new Response("Safari benchmark complete.");
    }
    return new Response(
      '<body style="margin:0;background:black;overflow:hidden"><canvas style="display:block;width:100vw;height:100vh;image-rendering:pixelated"></canvas>' +
        '<div style="position:fixed;bottom:3%;left:4%;background:black;color:white;padding:.5rem;font:20px system-ui">nanoctl latency marker</div>',
      { headers: { "content-type": "text/html" } },
    );
  },
});

let agent: ReturnType<typeof Bun.spawn> | undefined;
let browser: Awaited<ReturnType<typeof chromium.launch>> | undefined;
let instrumented = false;
try {
  await mkdir(resultsPath, { recursive: true });
  await command(["bun", "run", "build:viewer"]);
  const result = await Bun.build({
    entrypoints: [join(import.meta.dir, "safari.ts")],
    target: "browser",
  });
  if (!result.success) throw new AggregateError(result.logs, "Could not build Safari probe");
  const index = join(dist, "index.html");
  instrumented = true;
  await Bun.write(join(dist, "ios-benchmark.js"), result.outputs[0]);
  await Bun.write(
    index,
    (await Bun.file(index).text()).replace(
      "</head>",
      '<script type="module" src="/ios-benchmark.js"></script></head>',
    ),
  );
  agent = Bun.spawn(["bun", "tests/browser/agent.ts"], {
    cwd: root,
    env: { ...process.env, NANOCTL_BROWSER_PORT: "7337", NANOCTL_BROWSER_LATENCY: "1" },
    stdout: "inherit",
    stderr: "inherit",
  });
  await wait(
    async () => {
      try {
        const response = await fetch(endpoint + "/v1/info");
        if (!agent || agent.exitCode !== null || !response.ok) return;
        const info = await response.json();
        if (!info.remoteInput)
          throw new Error("Grant Screen Recording and Accessibility access to the agent");
        const asset = await fetch(endpoint + "/ios-benchmark.js");
        if (asset.ok) return true;
      } catch (error) {
        if (error instanceof Error && error.message.startsWith("Grant")) throw error;
      }
    },
    240,
    "instrumented agent",
  );

  await sim("install", device, resolve(app));
  const container = await sim("get_app_container", device, bundle, "data");
  const nativeReport = join(container, "Documents/latency.json");
  browser = await chromium.launch({
    headless: false,
    executablePath:
      process.env.NANOCTL_CHROME ?? "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    args: ["--kiosk"],
  });
  const page = await browser.newPage({ viewport: null });
  const startMarker = async () => {
    await page.goto("http://127.0.0.1:7338");
    await page.bringToFront();
    await page.locator("canvas").click();
    await page.evaluate((encode) => {
      const bits = new Function(`return (${encode})`)() as typeof markerBits;
      const canvas = document.querySelector("canvas")!;
      canvas.width = 44 * 20;
      canvas.height = 4;
      const ctx = canvas.getContext("2d")!;
      ctx.fillStyle = "cyan";
      ctx.fillRect(0, 0, 80, 4);
      ctx.fillStyle = "magenta";
      ctx.fillRect(800, 0, 80, 4);
      const marker = { paintTimes: [] as number[], input: 0 };
      (window as HostWindow).nanoctlMarker = marker;
      window.addEventListener("keydown", (event) => {
        if (event.isTrusted && event.code === "Space" && !event.repeat) {
          event.preventDefault();
          marker.input += 1;
        }
      });
      let previousTick = -1;
      const draw = (now: number) => {
        requestAnimationFrame(draw);
        const tick = Math.floor((now * 30) / 1000);
        if (tick === previousTick) return;
        previousTick = tick;
        const bars = bits(marker.paintTimes.length, marker.input);
        marker.paintTimes.push(performance.timeOrigin + performance.now());
        for (const [i, bar] of bars.entries()) {
          ctx.fillStyle = bar ? "white" : "black";
          ctx.fillRect((i + 4) * 20, 0, 20, 4);
        }
      };
      requestAnimationFrame(draw);
    }, markerBits.toString());
  };

  const run = async (label: "native" | "safari") => {
    await ignore(sim("terminate", device, bundle));
    await ignore(sim("terminate", device, "com.apple.mobilesafari"));
    await startMarker();
    let result: Probe;
    console.log(`Measuring ${label}…`);
    if (label === "native") {
      await rm(nativeReport, { force: true });
      await command(["xcrun", "simctl", "launch", device, bundle], {
        ...process.env,
        SIMCTL_CHILD_NANOCTL_BENCHMARK_URL: endpoint,
        SIMCTL_CHILD_NANOCTL_RENDER_ON_VSYNC: process.env.NANOCTL_RENDER_ON_VSYNC ?? "0",
      });
      result = await wait(
        async () => {
          if (await Bun.file(nativeReport).exists())
            return Bun.file(nativeReport).json() as Promise<Probe>;
        },
        45,
        "native presentation sample",
      );
      if (process.env.NANOCTL_RTP_TRACE && result.receiverEnd?.ssrc !== undefined) {
        const capture = await Bun.file(process.env.NANOCTL_RTP_TRACE).arrayBuffer();
        const arrivals = rtpMarkerArrivals(capture, result.receiverEnd.ssrc);
        for (const frame of result.frames) {
          if (frame.stages) frame.receiveMs = arrivals.get(frame.stages.rtp);
        }
        if (!result.frames.some((frame) => frame.receiveMs !== undefined)) {
          throw new Error(
            "No native frames matched the RTP trace; check the selected loopback family",
          );
        }
      }
      await sim("io", device, "screenshot", join(resultsPath, "native.png"));
    } else {
      safariReport = undefined;
      await sim("openurl", device, endpoint + "/?ios-benchmark");
      const screenshot = join(resultsPath, "safari-pending.png");
      const deadline = performance.now() + 45_000;
      while (!safariReport && performance.now() < deadline) {
        await Bun.sleep(2000);
        if (safariReport) break;
        await sim("io", device, "screenshot", screenshot);
        if (!safariReport) await Bun.write(join(resultsPath, "safari.png"), Bun.file(screenshot));
      }
      await rm(screenshot, { force: true });
      if (!safariReport) throw new Error("Timed out: Safari presentation sample");
      result = safariReport;
    }
    const marker = await page.evaluate(() => (window as HostWindow).nanoctlMarker);
    await Bun.write(
      join(resultsPath, `${label}-raw.json`),
      JSON.stringify({ ...result, marker }, null, 2),
    );
    const seen = new Set<number>();
    const frames = result.frames.filter(({ sequence }) => {
      if (seen.has(sequence)) return false;
      seen.add(sequence);
      return true;
    });
    if (frames.length < 60 || result.inputTimes.length !== 12 || marker.input !== 12) {
      throw new Error(
        `${label}: not enough video or input samples (${frames.length} frames, ${result.inputTimes.length} sent, ${marker.input} received)`,
      );
    }
    const video = frames.map((frame) => frame.displayMs - marker.paintTimes[frame.sequence]);
    const input = result.inputTimes.map((time, i) => {
      const frame = frames.find((frame) => frame.input === i + 1 && frame.displayMs >= time);
      if (!frame) throw new Error(`${label}: missing response to input ${i + 1}`);
      return frame.displayMs - time;
    });
    if (video.some((ms) => !Number.isFinite(ms) || ms < 0))
      throw new Error(`${label}: invalid host timestamps`);
    const fps = ((frames.length - 1) * 1000) / (frames.at(-1)!.displayMs - frames[0].displayMs);
    return {
      width: result.width,
      height: result.height,
      fps,
      video: summarize(video),
      input: summarize(input),
      timingsMs: timingBreakdown(frames, marker.paintTimes, result),
      receiver: receiverAverages(result.receiverStart, result.receiverEnd),
    };
  };

  const order = process.env.NANOCTL_SAFARI_FIRST
    ? (["safari", "native"] as const)
    : (["native", "safari"] as const);
  const report = {} as Record<(typeof order)[number], Awaited<ReturnType<typeof run>>>;
  for (const label of order) report[label] = await run(label);
  if (report.native.width !== report.safari.width || report.native.height !== report.safari.height)
    throw new Error("Video sizes differ");
  await Bun.write(join(resultsPath, "latency.json"), JSON.stringify(report, null, 2) + "\n");
  console.log(report);
} finally {
  await ignore(sim("terminate", device, bundle));
  await ignore(sim("terminate", device, "com.apple.mobilesafari"));
  await browser?.close();
  agent?.kill("SIGTERM");
  if (agent) await agent.exited;
  host.stop();
  if (instrumented) await command(["bun", "run", "build:viewer"]);
}
