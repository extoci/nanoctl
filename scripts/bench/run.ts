import { chromium } from "@playwright/test";
import { dlopen, ptr } from "bun:ffi";
import { execFileSync, spawn } from "node:child_process";
import { mkdir, writeFile } from "node:fs/promises";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join, resolve } from "node:path";
import { markerBits } from "../../tests/browser/latency-marker";
import { analyze } from "./metrics";

const [product, hostDisplay, clientDisplay, pidText, outputText] = process.argv.slice(2);
if (!product || !hostDisplay || !clientDisplay || !pidText || !outputText) {
  throw new Error(
    "Usage: bun scripts/bench/run.ts PRODUCT HOST_DISPLAY CLIENT_DISPLAY HOST_PID OUTPUT_DIR; VIEWER_URL for browser viewers, otherwise attach to an existing native viewer. RUNTIME_PATHS is a colon-separated runtime file/directory list.",
  );
}
const rootPid = Number(pidText),
  trials = Number(process.env.TRIALS ?? 120);
if (
  !Number.isInteger(rootPid) ||
  rootPid < 1 ||
  !Number.isInteger(trials) ||
  trials < 100 ||
  trials > 250
)
  throw new Error("Invalid PID or trials (100..250)");
const output = resolve(outputText);
await mkdir(output, { recursive: true });
const helper = join(output, "pixels");
execFileSync("cc", [
  "-O2",
  "-Wall",
  "-Wextra",
  "-Werror",
  resolve("scripts/bench/pixels.c"),
  "-o",
  helper,
  "-lX11",
  "-lXext",
  "-lXtst",
]);
const libc = dlopen("libc.so.6", { clock_gettime: { args: ["i32", "ptr"], returns: "i32" } });
const clockBuffer = new BigInt64Array(2);
const mono = () => {
  if (libc.symbols.clock_gettime(1, ptr(clockBuffer)) !== 0)
    throw new Error("CLOCK_MONOTONIC unavailable");
  return Number(clockBuffer[0]) * 1000 + Number(clockBuffer[1]) / 1e6;
};
const pause = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));
const ticksPerSecond = Number(execFileSync("getconf", ["CLK_TCK"], { encoding: "utf8" }).trim());
const pageKiB = Number(execFileSync("getconf", ["PAGESIZE"], { encoding: "utf8" }).trim()) / 1024;
function processes() {
  const all = new Map<number, { parent: number; ticks: number; rssKiB: number }>();
  for (const name of readdirSync("/proc")) {
    if (!/^\d+$/.test(name)) continue;
    try {
      const stat = readFileSync(`/proc/${name}/stat`, "utf8");
      const fields = stat.slice(stat.lastIndexOf(")") + 2).split(" ");
      all.set(Number(name), {
        parent: Number(fields[1]),
        ticks: Number(fields[11]) + Number(fields[12]),
        rssKiB: Number(fields[21]) * pageKiB,
      });
    } catch {
      /* Processes may exit between directory enumeration and reading. */
    }
  }
  const selected = new Set([rootPid]);
  let changed = true;
  while (changed) {
    changed = false;
    for (const [pid, p] of all)
      if (selected.has(p.parent) && !selected.has(pid)) {
        selected.add(pid);
        changed = true;
      }
  }
  return new Map([...all].filter(([pid]) => selected.has(pid)));
}
if (!processes().has(rootPid)) throw new Error("Host PID not found");
let phase = "warmup",
  lastTime = mono(),
  previous = processes();
const resources: {
  phase: string;
  ms: number;
  cpuPercent: number;
  rssKiB: number;
  pids: number[];
}[] = [];
const sampler = setInterval(() => {
  const time = mono(),
    current = processes();
  let delta = 0,
    rssKiB = 0;
  for (const [pid, p] of current) {
    delta += Math.max(0, p.ticks - (previous.get(pid)?.ticks ?? p.ticks));
    rssKiB += p.rssKiB;
  }
  resources.push({
    phase,
    ms: time - lastTime,
    cpuPercent: ((delta / ticksPerSecond) * 100_000) / (time - lastTime),
    rssKiB,
    pids: [...current.keys()],
  });
  lastTime = time;
  previous = current;
}, 250);
const hostBrowser = await chromium.launch({
  headless: false,
  env: { ...process.env, DISPLAY: hostDisplay },
  args: ["--kiosk", "--no-first-run", "--disable-dev-shm-usage"],
});
const host = await hostBrowser.newPage({ viewport: null });
await host.setContent(
  '<body style="margin:0;background:black;overflow:hidden"><canvas width="1280" height="720"></canvas></body>',
);
await host.locator("canvas").click();
await host.evaluate(() => {
  document.title = "nanoctl-bench-source";
});
const hostWindow = execFileSync(
  "xdotool",
  ["search", "--sync", "--onlyvisible", "--name", "^nanoctl-bench-source"],
  { env: { ...process.env, DISPLAY: hostDisplay }, encoding: "utf8", timeout: 10_000 },
)
  .trim()
  .split("\n")
  .at(-1)!;
execFileSync(
  "xdotool",
  [
    "windowmove",
    hostWindow,
    "0",
    "0",
    "windowsize",
    hostWindow,
    "1280",
    "720",
    "windowfocus",
    hostWindow,
  ],
  { env: { ...process.env, DISPLAY: hostDisplay } },
);
await host.evaluate((encode) => {
  const bits = new Function(`return (${encode})`)() as typeof markerBits;
  const canvas = document.querySelector("canvas")!;
  const ctx = canvas.getContext("2d")!;
  const state = { paints: [] as number[], input: 0, motion: false, keys: [] as unknown[] };
  Object.assign(window, { bench: state });
  let dirty = true;
  window.addEventListener("keydown", (e) => {
    state.keys.push({ code: e.code, key: e.key, trusted: e.isTrusted, repeat: e.repeat });
    if (e.isTrusted && e.code === "Space" && !e.repeat) {
      e.preventDefault();
      state.input++;
      dirty = true;
    }
  });
  const render = () => {
    if (state.motion || dirty) {
      const seq = state.paints.length;
      ctx.fillStyle = "#151515";
      ctx.fillRect(0, 0, 1280, 720);
      ctx.fillStyle = "cyan";
      ctx.fillRect(0, 100, 80, 100);
      ctx.fillStyle = "magenta";
      ctx.fillRect(800, 100, 80, 100);
      bits(seq, state.input).forEach((bit, i) => {
        ctx.fillStyle = bit ? "white" : "black";
        ctx.fillRect((i + 4) * 20, 100, 20, 100);
      });
      ctx.fillStyle = "white";
      ctx.font = "36px sans-serif";
      ctx.fillText(`frame ${seq} input ${state.input}`, 80, 300);
      if (state.motion) {
        ctx.fillStyle = "#38bdf8";
        ctx.fillRect((seq * 7) % 1120, 400, 160, 160);
      }
      state.paints.push(performance.now());
      dirty = false;
    }
    requestAnimationFrame(render);
  };
  requestAnimationFrame(render);
}, markerBits.toString());
let viewerBrowser: Awaited<ReturnType<typeof chromium.launch>> | undefined;
let viewer: Awaited<ReturnType<typeof hostBrowser.newPage>> | undefined;
const stats: unknown[] = [];
const viewerSelector = process.env.VIEWER_SELECTOR ?? "video";
let clientWindow = process.env.BENCH_WINDOW;
if (process.env.VIEWER_URL) {
  viewerBrowser = await chromium.launch({
    headless: false,
    env: { ...process.env, DISPLAY: clientDisplay },
    args: ["--kiosk", "--no-first-run", "--disable-dev-shm-usage"],
  });
  const context = await viewerBrowser.newContext({ viewport: null });
  await context.addInitScript(() => {
    const Original = window.RTCPeerConnection;
    const peers: RTCPeerConnection[] = [];
    Object.assign(window, { benchPeers: peers });
    const keys: unknown[] = [];
    Object.assign(window, { benchKeys: keys });
    window.addEventListener(
      "keydown",
      (e) => keys.push({ code: e.code, key: e.key, trusted: e.isTrusted, repeat: e.repeat }),
      true,
    );
    window.RTCPeerConnection = new Proxy(Original, {
      construct(target, args) {
        const peer = new target(...args);
        peers.push(peer);
        return peer;
      },
    });
  });
  viewer = await context.newPage();
  await viewer.goto(process.env.VIEWER_URL);
  await viewer.locator(viewerSelector).waitFor();
  await viewer.waitForFunction((selector) => {
    const element = document.querySelector(selector);
    return element instanceof HTMLVideoElement
      ? element.videoWidth > 0
      : !!element && element.getBoundingClientRect().width > 0;
  }, viewerSelector);
}
if (viewer) {
  await viewer.evaluate(() => {
    document.title = "nanoctl-bench-client";
  });
  clientWindow = execFileSync(
    "xdotool",
    ["search", "--sync", "--onlyvisible", "--name", "^nanoctl-bench-client"],
    { env: { ...process.env, DISPLAY: clientDisplay }, encoding: "utf8", timeout: 10_000 },
  )
    .trim()
    .split("\n")
    .at(-1)!;
  execFileSync(
    "xdotool",
    [
      "windowmove",
      clientWindow,
      "0",
      "0",
      "windowsize",
      clientWindow,
      "1280",
      "720",
      "windowfocus",
      clientWindow,
    ],
    { env: { ...process.env, DISPLAY: clientDisplay } },
  );
  await viewer.locator(viewerSelector).focus();
}
await pause(4000);
execFileSync("xdotool", ["mousemove", "--sync", "640", "360", "click", "1"], {
  env: { ...process.env, DISPLAY: clientDisplay },
});
const detectorEnv = { ...process.env, ...(clientWindow ? { BENCH_WINDOW: clientWindow } : {}) };
const idleRaw = execFileSync(helper, [clientDisplay, "0.1", "0", "1"], {
  encoding: "utf8",
  env: detectorEnv,
});
if (!idleRaw.split("\n").some((line) => /^S \S+ 0 0 /.test(line))) {
  await host.screenshot({ path: join(output, `${product}.host-error.png`) });
  await viewer?.screenshot({ path: join(output, `${product}.client-error.png`) });
  throw new Error(
    `Client marker not decodable; check geometry and stream: ${idleRaw.slice(0, 300)}`,
  );
}
phase = "idle";
await pause(10_000);
phase = "warmup";
let best = { offsetMs: 0, uncertaintyMs: Infinity };
for (let i = 0; i < 20; i++) {
  const t0 = mono(),
    browserMs = await host.evaluate(() => performance.now()),
    t1 = mono();
  if ((t1 - t0) / 2 < best.uncertaintyMs)
    best = { offsetMs: (t0 + t1) / 2 - browserMs, uncertaintyMs: (t1 - t0) / 2 };
}
await host.evaluate(() => {
  (window as unknown as { bench: { motion: boolean } }).bench.motion = true;
});
phase = "motion";
const duration = Math.ceil(2 + trials * 0.45 + 3);
const capture = spawn(helper, [clientDisplay, String(duration), String(trials), "1729"], {
  stdio: ["ignore", "pipe", "inherit"],
  env: detectorEnv,
});
let raw = "";
capture.stdout.on("data", (chunk) => {
  raw += chunk;
});
const statsTimer = viewer
  ? setInterval(async () => {
      try {
        stats.push(
          await viewer!.evaluate(async () => {
            const peers = (window as unknown as { benchPeers: RTCPeerConnection[] }).benchPeers;
            const entries = [];
            for (const p of peers) {
              const report = await p.getStats();
              for (const value of report.values())
                if (value.type === "inbound-rtp" && value.kind === "video") entries.push(value);
            }
            return entries;
          }),
        );
      } catch {
        /* The browser may be closing. */
      }
    }, 1000)
  : undefined;
const code = await new Promise<number | null>((resolve, reject) => {
  capture.once("error", reject);
  capture.once("exit", resolve);
});
if (statsTimer) clearInterval(statsTimer);
clearInterval(sampler);
if (code !== 0) throw new Error(`Detector failed: ${code}`);
const state = await host.evaluate(
  () =>
    (window as unknown as { bench: { paints: number[]; input: number; motion: boolean } }).bench,
);
await host.evaluate(() => {
  (window as unknown as { bench: { motion: boolean } }).bench.motion = false;
});
const metrics = analyze(raw, state.paints, best.offsetMs);
const received = (
  stats.flat() as { bytesReceived: number; timestamp: number; ssrc: number }[]
).filter((s) => Number.isFinite(s.bytesReceived));
const streams = [...new Set(received.map((s) => s.ssrc))];
const receivedMediaMbps = streams.map((ssrc) => {
  const rows = received.filter((s) => s.ssrc === ssrc);
  const first = rows[0],
    last = rows.at(-1)!;
  return {
    ssrc,
    mbps:
      ((last.bytesReceived - first.bytesReceived) * 8) / (last.timestamp - first.timestamp) / 1000,
  };
});
function resourceSummary(label: string) {
  const rows = resources.filter((r) => r.phase === label);
  const elapsed = rows.reduce((sum, r) => sum + r.ms, 0);
  return {
    samples: rows.length,
    seconds: elapsed / 1000,
    cpuPercent: rows.reduce((sum, r) => sum + r.cpuPercent * r.ms, 0) / elapsed,
    rssMiB: rows.reduce((sum, r) => sum + r.rssKiB * r.ms, 0) / elapsed / 1024,
  };
}
const seen = new Set<string>();
function bytes(path: string): number {
  const resolved = resolve(path);
  if (seen.has(resolved)) return 0;
  seen.add(resolved);
  const stat = statSync(resolved);
  return stat.isDirectory()
    ? readdirSync(resolved).reduce((sum, name) => sum + bytes(join(resolved, name)), 0)
    : stat.size;
}
const paths = (process.env.RUNTIME_PATHS ?? "").split(":").filter(Boolean);
const result = {
  schemaVersion: 1,
  product,
  date: new Date().toISOString(),
  hostDisplay,
  clientDisplay,
  rootPid,
  settings: {
    width: 1280,
    height: 720,
    encoder: "software H.264",
    bitrate: process.env.BITRATE_NOTE ?? "not supplied",
    trials,
    seed: 1729,
    chrome: hostBrowser.version(),
  },
  source: {
    frames: state.paints.length,
    inputs: state.input,
    clock: "CLOCK_MONOTONIC via libc clock_gettime",
    ...best,
  },
  ...metrics,
  receivedMediaMbps,
  resources: { idle: resourceSummary("idle"), motion: resourceSummary("motion") },
  runtime: {
    paths,
    bytes: paths.length ? paths.reduce((sum, path) => sum + bytes(path), 0) : null,
    excludes: "build caches and system shared libraries",
  },
  raw: {
    paints: state.paints,
    resources,
    webRtcStats: stats,
    viewerKeys: viewer
      ? await viewer.evaluate(() => (window as unknown as { benchKeys: unknown[] }).benchKeys)
      : null,
  },
  valid:
    metrics.inputMs.count >= 100 &&
    metrics.missingInputs.length === 0 &&
    metrics.detector.hz >= 200 &&
    state.input === trials &&
    metrics.uniqueFrames >= 60 &&
    metrics.videoMs.min !== null &&
    metrics.videoMs.min >= -best.uncertaintyMs,
};
const f = (n: number | null) => (n === null ? "n/a" : n.toFixed(2));
const row = `| ${product} | ${metrics.inputMs.count}/${trials} | ${f(metrics.inputMs.p50)} / ${f(metrics.inputMs.p95)} / ${f(metrics.inputMs.p99)} | ${f(metrics.videoMs.p50)} / ${f(metrics.videoMs.p95)} / ${f(metrics.videoMs.p99)} | ${f(metrics.uniqueFps)} | ${f(metrics.interFrameMs.p50)} / ${f(metrics.interFrameMs.p95)} / ${f(metrics.interFrameMs.p99)} | ${metrics.stallsOver100Ms} | ${f(result.resources.idle.cpuPercent)} / ${f(result.resources.motion.cpuPercent)} | ${f(result.resources.idle.rssMiB)} / ${f(result.resources.motion.rssMiB)} | ${f(result.runtime.bytes === null ? null : result.runtime.bytes / 1048576)} |`;
const markdown = `| Product | Inputs | Input p50/p95/p99 ms | Video p50/p95/p99 ms | Unique fps | Frame interval p50/p95/p99 ms | Stalls >100ms | CPU idle/motion % | RSS idle/motion MiB | Runtime MiB |\n| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |\n${row}\n\nDetector: ${f(metrics.detector.hz)} Hz, ${metrics.detector.samples} polls; polling p99 ${f(metrics.detector.pollingMs.p99)} ms; read p99 ${f(metrics.detector.readMs.p99)} ms; CPU ${f(metrics.detector.cpuPercent)}%; clock uncertainty ±${f(best.uncertaintyMs)} ms. Valid: ${result.valid}.\n`;
await writeFile(join(output, `${product}.json`), JSON.stringify(result, null, 2));
await writeFile(join(output, `${product}.pixels.log`), raw);
await writeFile(join(output, `${product}.md`), markdown);
console.log(markdown);
await writeFile(join(output, `${product}.source.json`), JSON.stringify(state));
await viewerBrowser?.close();
await hostBrowser.close();
if (!result.valid) process.exitCode = 1;
