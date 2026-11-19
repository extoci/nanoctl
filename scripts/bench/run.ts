import { chromium } from "@playwright/test";
import { dlopen, ptr } from "bun:ffi";
import { execFileSync, spawn, spawnSync } from "node:child_process";
import { mkdir, writeFile } from "node:fs/promises";
import { existsSync, readFileSync, readdirSync } from "node:fs";
import { join, resolve } from "node:path";
import { markerBits, readMarker, readMarkerRow } from "../../tests/browser/latency-marker";
import { analyze } from "./metrics";
import { repeatArgs, summarizeRuns, summaryMarkdown, type BenchRun } from "./repeat";
import { runtimeBytes } from "./runtime";
import { setupClicks } from "./viewer";

const options = repeatArgs(process.argv.slice(2));
const viewerSetupClicks = setupClicks(process.env.VIEWER_SETUP_CLICKS);
const [product, hostDisplay, clientDisplay, pidText, outputText] = options.args;
if (!product || !hostDisplay || !clientDisplay || !pidText || !outputText) {
  throw new Error(
    "Usage: bun scripts/bench/run.ts PRODUCT HOST_DISPLAY CLIENT_DISPLAY HOST_PID OUTPUT_DIR [--repeat N]; VIEWER_URL for browser viewers, otherwise attach to an existing native viewer. RUNTIME_PATHS is a colon-separated runtime file/directory list.",
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
if (options.repeat !== null) {
  for (let i = 1; i <= options.repeat; i++) {
    const file = join(output, `${product}-run-${i}.json`);
    if (existsSync(file)) throw new Error(`Refusing to overwrite repeat artifact: ${file}`);
  }
  const runs: BenchRun[] = [];
  const files: string[] = [];
  const failures: string[] = [];
  for (let i = 1; i <= options.repeat; i++) {
    const name = `${product}-run-${i}`;
    const child = spawnSync(
      process.execPath,
      [import.meta.path, name, hostDisplay, clientDisplay, pidText, output],
      { stdio: "inherit", env: process.env },
    );
    const file = join(output, `${name}.json`);
    files.push(file);
    try {
      runs.push(JSON.parse(readFileSync(file, "utf8")) as BenchRun);
    } catch (error) {
      failures.push(`${file}: ${String(error)}`);
    }
    if (child.status !== 0)
      failures.push(
        `${name}: exit ${child.status}, signal ${child.signal}, error ${child.error ?? "none"}`,
      );
  }
  const summary = { ...summarizeRuns(product, runs, options.repeat), files, failures };
  summary.complete &&= failures.length === 0;
  await writeFile(join(output, `${product}.repeat.json`), JSON.stringify(summary, null, 2));
  const markdown = summaryMarkdown(summary);
  await writeFile(join(output, `${product}.repeat.md`), markdown);
  console.log(markdown);
  process.exit(summary.complete ? 0 : 1);
}
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
  const state = {
    paints: [] as number[],
    input: 0,
    motion: false,
    keys: [] as unknown[],
    inputPaints: [] as { input: number; at: number }[],
  };
  Object.assign(window, { bench: state });
  let dirty = true;
  window.addEventListener("keydown", (e) => {
    state.keys.push({
      at: performance.now(),
      code: e.code,
      key: e.key,
      trusted: e.isTrusted,
      repeat: e.repeat,
    });
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
      if (state.inputPaints.at(-1)?.input !== state.input)
        state.inputPaints.push({ input: state.input, at: state.paints.at(-1)! });
      dirty = false;
    }
    requestAnimationFrame(render);
  };
  requestAnimationFrame(render);
}, markerBits.toString());
let viewerBrowser: Awaited<ReturnType<typeof chromium.launch>> | undefined;
let viewer: Awaited<ReturnType<typeof hostBrowser.newPage>> | undefined;
const stats: unknown[] = [];
let viewerClock = { offsetMs: 0, uncertaintyMs: Infinity };
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
      (e) =>
        keys.push({
          at: performance.now(),
          code: e.code,
          key: e.key,
          trusted: e.isTrusted,
          repeat: e.repeat,
        }),
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
  for (const selector of viewerSetupClicks) await viewer.locator(selector).click();
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
if (viewer && process.env.BENCH_ATTRIBUTION === "1") {
  for (let i = 0; i < 20; i++) {
    const t0 = mono(),
      at = await viewer.evaluate(() => performance.now()),
      t1 = mono();
    if ((t1 - t0) / 2 < viewerClock.uncertaintyMs)
      viewerClock = { offsetMs: (t0 + t1) / 2 - at, uncertaintyMs: (t1 - t0) / 2 };
  }
  await viewer.evaluate(
    ({ marker, row }) => {
      const decode = new Function("readMarker", `return (${row})`)(
        new Function(`return (${marker})`)(),
      ) as typeof readMarkerRow;
      const state = { frames: [] as object[], sends: [] as object[] };
      Object.assign(window, { benchAttribution: state });
      const original = RTCDataChannel.prototype.send;
      RTCDataChannel.prototype.send = new Proxy(original, {
        apply(target, self, args) {
          if (typeof args[0] === "string") {
            try {
              const m = JSON.parse(args[0]);
              if (m.type === "key" && m.action === "down" && m.code === "Space")
                state.sends.push({ at: performance.now() });
            } catch {
              /* Not a JSON control message. */
            }
          }
          return Reflect.apply(target, self, args);
        },
      });
      const video = document.querySelector("video")!;
      const canvas = document.createElement("canvas");
      canvas.width = video.videoWidth;
      canvas.height = 1;
      const ctx = canvas.getContext("2d", { willReadFrequently: true })!;
      let markerY: number | undefined;
      const sample: VideoFrameRequestCallback = (at, metadata) => {
        let id;
        if (markerY === undefined) {
          canvas.height = video.videoHeight;
          ctx.drawImage(video, 0, 0);
          for (let y = 0; y < canvas.height && !id; y += 4) {
            id = decode(ctx.getImageData(0, y, canvas.width, 1).data);
            if (id) markerY = y;
          }
          canvas.height = 1;
        } else {
          ctx.drawImage(video, 0, markerY, video.videoWidth, 1, 0, 0, canvas.width, 1);
          id = decode(ctx.getImageData(0, 0, canvas.width, 1).data);
        }
        if (id && state.frames.length < 30_000) state.frames.push({ ...id, at, ...metadata });
        video.requestVideoFrameCallback(sample);
      };
      video.requestVideoFrameCallback(sample);
    },
    { marker: readMarker.toString(), row: readMarkerRow.toString() },
  );
}
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
    viewerSelector,
    viewerSetupClicks,
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
    bytes: paths.length ? runtimeBytes(paths) : null,
    excludes: process.env.RUNTIME_EXCLUDES ?? "build caches and system shared libraries",
  },
  raw: {
    attribution:
      viewer && process.env.BENCH_ATTRIBUTION === "1"
        ? {
            clock: viewerClock,
            ...(await viewer.evaluate(
              () => (window as unknown as { benchAttribution: object }).benchAttribution,
            )),
          }
        : null,
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
