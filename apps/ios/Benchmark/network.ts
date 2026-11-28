import { chromium } from "@playwright/test";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const root = resolve(import.meta.dir, "../../..");
const [app, device, output, only] = process.argv.slice(2);
if (!app || !device || !output)
  throw new Error(
    "Usage: bun apps/ios/Benchmark/network.ts APP_PATH SIMULATOR_ID OUTPUT_DIR [scenario]",
  );

type Impairment = {
  lossPpm?: number;
  reorderPpm?: number;
  jitterMs?: number;
  bandwidthKbps?: number;
};
type Scenario = {
  durationMs: number;
  impairment: Impairment;
  restartAtMs?: number;
  restartDownMs?: number;
  minFps: number;
  maxGapMs: number;
  description: string;
  codec?: "h264" | "av1";
  av1Fixture?: boolean;
  minScreenshotLuma?: number;
};
const scenarios: Record<string, Scenario> = {
  clean: {
    durationMs: 12_000,
    impairment: {},
    minFps: 15,
    maxGapMs: 700,
    description: "clean loopback control",
  },
  av1: {
    durationMs: 15_000,
    impairment: {},
    minFps: 7,
    maxGapMs: 700,
    description:
      "clean loopback AV1 IVF fixture through the real RTP path plus a VideoToolbox bitstream proof",
    codec: "av1",
    av1Fixture: true,
    minScreenshotLuma: 25,
  },
  "lossy-wifi": {
    durationMs: 20_000,
    impairment: { lossPpm: 20_000, reorderPpm: 20_000, jitterMs: 25, bandwidthKbps: 20_000 },
    minFps: 7,
    maxGapMs: 1_500,
    description: "2% loss, 2% long-tail reorder, up to 25ms jitter, 20 Mbps",
  },
  congested: {
    durationMs: 25_000,
    impairment: { lossPpm: 50_000, reorderPpm: 100_000, jitterMs: 35, bandwidthKbps: 3_000 },
    minFps: 4,
    maxGapMs: 2_500,
    description: "5% loss, 10% long-tail reorder, up to 35ms jitter, 3 Mbps",
  },
  "service-restart": {
    durationMs: 25_000,
    impairment: {},
    restartAtMs: 5_000,
    restartDownMs: 5_000,
    minFps: 8,
    maxGapMs: 1_500,
    description: "agent process stops for 5s mid-session, then returns",
  },
};
const selected = only ? [only] : Object.keys(scenarios);
for (const name of selected) if (!scenarios[name]) throw new Error(`Unknown scenario ${name}`);

type Report = {
  renders: number[];
  states: { at: number; state: string }[];
  samples: {
    at: number;
    state: string;
    transport: string;
    codec?: string;
    inputReady: boolean;
    renders: number;
    dcMessages: number;
    dcFed: number;
    dcDecoded: number;
    dcDropped: number;
    dcGaps: number;
    dcKeyframeRequests: number;
    dcFaulted: boolean;
  }[];
};

const endpoint = "http://127.0.0.1:7337";
const bundle = "ai.nanoctl.viewer";
const resultsPath = resolve(output);
await mkdir(resultsPath, { recursive: true });

async function runCommand(args: string[], env?: NodeJS.ProcessEnv) {
  const child = Bun.spawn(args, { cwd: root, env, stdout: "pipe", stderr: "inherit" });
  const out = await new Response(child.stdout).text();
  if ((await child.exited) !== 0) throw new Error(`${args.join(" ")} failed`);
  return out.trim();
}
const sim = (...args: string[]) => runCommand(["xcrun", "simctl", ...args]);
async function runCombinedOutput(args: string[]) {
  const child = Bun.spawn(args, { cwd: root, stdout: "pipe", stderr: "pipe" });
  const [out, err, code] = await Promise.all([
    new Response(child.stdout).text(),
    new Response(child.stderr).text(),
    child.exited,
  ]);
  if (code !== 0) throw new Error(`${args.join(" ")} failed`);
  return out + err;
}
const wait = async <T>(probe: () => Promise<T | undefined>, seconds: number, label: string) => {
  const deadline = performance.now() + seconds * 1000;
  while (performance.now() < deadline) {
    const result = await probe();
    if (result !== undefined) return result;
    await Bun.sleep(200);
  }
  throw new Error(`Timed out: ${label}`);
};

await runCommand([
  "cargo",
  "build",
  "--release",
  "--features",
  "media",
  "--manifest-path",
  "crates/nanoctl-agent/Cargo.toml",
]);

const markerBrowser = await chromium.launch({
  headless: false,
  executablePath:
    process.env.NANOCTL_CHROME ?? "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
  args: ["--kiosk"],
});
const failures: string[] = [];
const summaries: Record<string, unknown> = {};
try {
  const marker = await markerBrowser.newPage({ viewport: null });
  await marker.setContent(
    '<body style="margin:0;background:black;overflow:hidden"><canvas style="display:block;width:100vw;height:100vh"></canvas></body>',
  );
  await marker.evaluate(() => {
    const canvas = document.querySelector("canvas")!;
    canvas.width = 960;
    canvas.height = 540;
    const context = canvas.getContext("2d")!;
    let paints = 0;
    const render = () => {
      paints += 1;
      context.fillStyle = "black";
      context.fillRect(0, 0, canvas.width, canvas.height);
      for (let index = 0; index < 28; index += 1) {
        const x = (paints * (4 + (index % 5)) + index * 173) % canvas.width;
        const y = (paints * (3 + (index % 3)) + index * 97) % canvas.height;
        context.fillStyle = `hsl(${(paints * 7 + index * 31) % 360} 90% 55%)`;
        context.fillRect(x, y, 100 + (index % 4) * 30, 42 + (index % 3) * 24);
      }
      requestAnimationFrame(render);
    };
    requestAnimationFrame(render);
  });

  for (const name of selected) {
    const scenario = scenarios[name];
    const directory = await mkdtemp(join(tmpdir(), "nanoctl-ios-network-"));
    const config = join(directory, "config.toml");
    await writeFile(
      config,
      `[tailnet]\nport = 7337\ntls = "off"\nallow_users = ["dev@example.com"]\nserve_loopback = false\n[quality]\nencoder = "software"\nmax_width = 1280\nmax_height = 720\nmax_fps = 30\nmax_bitrate_kbps = 24000\n`,
    );
    const env: NodeJS.ProcessEnv = {
      ...process.env,
      NANOCTL_DEV_LOOPBACK_USER: "dev@example.com",
      XDG_DATA_HOME: directory,
      XDG_CONFIG_HOME: directory,
    };
    if (scenario.av1Fixture) {
      const fixture = join(directory, "av1-fixture.ivf");
      await runCommand([
        "ffmpeg",
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=320x180:rate=10",
        "-t",
        "5",
        "-c:v",
        "libsvtav1",
        "-g",
        "5",
        "-crf",
        "35",
        "-preset",
        "8",
        "-pix_fmt",
        "yuv420p",
        "-svtav1-params",
        "lookahead=0:hierarchical-levels=0:enable-tf=0",
        "-f",
        "ivf",
        fixture,
      ]);
      env.NANOCTL_TEST_AV1_IVF = fixture;
      const proofBinary = join(directory, "av1-video-toolbox-proof");
      await runCommand([
        "swiftc",
        "-O",
        resolve(root, "apps/ios/Benchmark/av1-video-toolbox-proof.swift"),
        resolve(root, "apps/ios/Nanoctl/AV1Bitstream.swift"),
        "-o",
        proofBinary,
      ]);
      console.log(`av1-video-toolbox: ${await runCommand([proofBinary, fixture])}`);
    }
    const impairment = scenario.impairment;
    if (impairment.lossPpm) env.NANOCTL_TEST_VIDEO_LOSS_PPM = String(impairment.lossPpm);
    if (impairment.reorderPpm) env.NANOCTL_TEST_VIDEO_REORDER_PPM = String(impairment.reorderPpm);
    if (impairment.jitterMs) env.NANOCTL_TEST_VIDEO_JITTER_MS = String(impairment.jitterMs);
    if (impairment.bandwidthKbps)
      env.NANOCTL_TEST_VIDEO_BANDWIDTH_KBPS = String(impairment.bandwidthKbps);
    const launchAgent = () =>
      Bun.spawn(
        [join(root, "crates/nanoctl-agent/target/release/nanoctl"), "--config", config, "run"],
        {
          cwd: root,
          env,
          stdout: "inherit",
          stderr: "inherit",
        },
      );
    let agent = launchAgent();
    try {
      const info = await wait(
        async () => {
          try {
            const response = await fetch(endpoint + "/v1/info");
            if (response.ok) return response.json();
          } catch {}
          return undefined;
        },
        20,
        `${name} agent`,
      );
      if (!info.capabilities?.ready) {
        throw new Error(
          `${name} agent is not media/RTC-ready (${info.capabilities?.readiness ?? "unknown"}); rebuild it with cargo build --release --locked --features media --manifest-path crates/nanoctl-agent/Cargo.toml`,
        );
      }
      await runCommand(["xcrun", "simctl", "terminate", device, bundle]).catch(() => "");
      await sim("uninstall", device, bundle).catch(() => "");
      await sim("install", device, resolve(app));
      await sim(
        "spawn",
        device,
        "defaults",
        "write",
        bundle,
        "codec",
        "-string",
        scenario.codec ?? "h264",
      );
      const container = await sim("get_app_container", device, bundle, "data");
      const reportPath = join(container, "Documents/network-probe.json");
      await rm(reportPath, { force: true });
      await runCommand(["xcrun", "simctl", "launch", device, bundle], {
        ...process.env,
        SIMCTL_CHILD_NANOCTL_NETWORK_PROBE_URL: endpoint,
      });
      if (scenario.restartAtMs) {
        await Bun.sleep(scenario.restartAtMs);
        agent.kill("SIGINT");
        await agent.exited.catch(() => 0);
        await Bun.sleep(scenario.restartDownMs ?? 0);
        agent = launchAgent();
        await Bun.sleep(
          Math.max(0, scenario.durationMs - scenario.restartAtMs - (scenario.restartDownMs ?? 0)),
        );
      } else {
        await Bun.sleep(scenario.durationMs);
      }
      const report = await wait(
        async () => {
          if (await Bun.file(reportPath).exists())
            return Bun.file(reportPath).json() as Promise<Report>;
        },
        10,
        `${name} network probe`,
      );
      const screenshotPath = join(resultsPath, `${name}.png`);
      await sim("io", device, "screenshot", screenshotPath).catch(() => "");
      if (scenario.minScreenshotLuma) {
        const stats = await runCombinedOutput([
          "ffmpeg",
          "-hide_banner",
          "-v",
          "info",
          "-i",
          screenshotPath,
          "-vf",
          "signalstats,metadata=print",
          "-f",
          "null",
          "-",
        ]);
        const luma = Number(stats.match(/lavfi\.signalstats\.YAVG=([0-9.]+)/)?.[1] ?? 0);
        if (luma < scenario.minScreenshotLuma) {
          failures.push(
            `${name}: screenshot luma ${luma.toFixed(1)} < ${scenario.minScreenshotLuma}`,
          );
        }
      }

      const last = report.samples.at(-1);
      const connectingAt =
        report.states.find((state) => state.state.startsWith("Connecting"))?.at ?? 0;
      const firstLiveAt = report.states.find((state) => state.state.startsWith("Live"))?.at;
      const lastLiveAt = report.states.findLast((state) => state.state.startsWith("Live"))?.at;
      const connectMs = firstLiveAt === undefined ? undefined : (firstLiveAt - connectingAt) * 1000;
      const analysisStart = Math.max(3, (lastLiveAt ?? 0) + 1);
      const renders = report.renders.filter((at) => at >= analysisStart);
      const elapsed = Math.max(1, scenario.durationMs / 1000 - analysisStart);
      let maxGapMs = renders.length ? (renders[0] - analysisStart) * 1000 : scenario.durationMs;
      for (let index = 1; index < renders.length; index += 1) {
        maxGapMs = Math.max(maxGapMs, (renders[index] - renders[index - 1]) * 1000);
      }
      const recoveryMs =
        scenario.restartAtMs && lastLiveAt !== undefined
          ? Math.max(0, lastLiveAt * 1000 - scenario.restartAtMs)
          : undefined;
      const summary = {
        description: scenario.description,
        impairment: scenario.impairment,
        durationMs: scenario.durationMs,
        connectMs,
        recoveryMs,
        renders: renders.length,
        fps: renders.length / elapsed,
        maxGapMs,
        state: last?.state,
        transport: last?.transport,
        codec: last?.codec,
        inputReady: last?.inputReady,
        dc: last && {
          messages: last.dcMessages,
          fed: last.dcFed,
          decoded: last.dcDecoded,
          dropped: last.dcDropped,
          gaps: last.dcGaps,
          keyframeRequests: last.dcKeyframeRequests,
          faulted: last.dcFaulted,
        },
        states: report.states,
      };
      summaries[name] = summary;
      await Bun.write(
        join(resultsPath, `${name}.json`),
        JSON.stringify({ summary, report }, null, 2),
      );
      console.log(
        `${name}: connect=${connectMs?.toFixed(0) ?? "?"}ms recovery=${recoveryMs?.toFixed(0) ?? "-"}ms fps=${summary.fps.toFixed(1)} max-gap=${maxGapMs.toFixed(0)}ms state=${last?.state} transport=${last?.transport}/${last?.codec} decoded=${last?.dcDecoded}`,
      );
      if (summary.fps < scenario.minFps)
        failures.push(`${name}: fps ${summary.fps.toFixed(1)} < ${scenario.minFps}`);
      if (maxGapMs > scenario.maxGapMs)
        failures.push(`${name}: max gap ${maxGapMs.toFixed(0)}ms > ${scenario.maxGapMs}ms`);
      if (!last?.inputReady) failures.push(`${name}: input channel is not ready`);
      if (!last?.state.startsWith("Live")) failures.push(`${name}: final state is ${last?.state}`);
      if (scenario.restartAtMs && recoveryMs === undefined)
        failures.push(`${name}: did not return to live video after the service restart`);
    } finally {
      agent.kill("SIGINT");
      await agent.exited.catch(() => 0);
      await runCommand(["xcrun", "simctl", "terminate", device, bundle]).catch(() => "");
      await rm(directory, { recursive: true, force: true });
    }
  }
} finally {
  await markerBrowser.close();
}
await Bun.write(join(resultsPath, "summary.json"), JSON.stringify(summaries, null, 2) + "\n");
if (failures.length) throw new Error(failures.join("\n"));
