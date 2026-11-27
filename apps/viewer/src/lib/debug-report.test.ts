import { afterEach, beforeEach, expect, test } from "bun:test";
import {
  buildDebugReport,
  fetchHostReport,
  recordDebugEvent,
  recordMetrics,
  resetDebugLog,
} from "./debug-report";
import type { RemoteSessionMetrics, RemoteSessionSnapshot } from "./remote-session";

const metrics: RemoteSessionMetrics = {
  resolution: "2560×1440",
  fps: 119.8,
  bitrateKbps: 24_000,
  rttMs: 4,
  packetsLost: 0,
  framesDropped: 1,
  lossPercent: 0,
  jitterMs: 1.2,
  route: "direct",
  codec: "AV1",
  decoder: "ExternalDecoder",
  decodeMs: 2.1,
};
const snapshot: RemoteSessionSnapshot = {
  phase: "connected",
  reconnectAttempt: 0,
  iceGeneration: 1,
  inputEnabled: true,
  gamepadEnabled: false,
  gamepadStatus: "waiting",
  firstFrameReceived: true,
  metrics,
  inputBlocked: 0,
};

const GLOBALS = ["location", "navigator", "screen", "window"] as const;
let saved: PropertyDescriptor[] = [];

beforeEach(() => {
  resetDebugLog();
  saved = GLOBALS.map((name) => Object.getOwnPropertyDescriptor(globalThis, name)!);
  const fakes = {
    location: { origin: "https://host.tail.ts.net:7337", pathname: "/" },
    navigator: { userAgent: "test-agent", hardwareConcurrency: 8 },
    screen: { width: 2560, height: 1440 },
    window: { isSecureContext: true, devicePixelRatio: 2 },
  };
  for (const [name, value] of Object.entries(fakes)) {
    Object.defineProperty(globalThis, name, { value, configurable: true, writable: true });
  }
});

afterEach(() => {
  GLOBALS.forEach((name, index) => {
    const descriptor = saved[index];
    if (descriptor) Object.defineProperty(globalThis, name, descriptor);
    else delete (globalThis as Record<string, unknown>)[name];
  });
});

test("bundles viewer state, stats, events and the host report", async () => {
  recordDebugEvent("socket online");
  recordDebugEvent("peer connected", "ice connected; generation 1");
  recordMetrics(metrics);
  const report = await buildDebugReport({
    info: null,
    snapshot,
    socketStatus: "online",
    socketDetail: "",
    codecChoice: "auto",
    hostReport: "# nanoctl debug report\nhost body",
    decodeSupport: async (codec) => ({
      supported: codec !== "hevc",
      powerEfficient: codec === "av1",
    }),
  });
  expect(report).toContain("- user agent: test-agent");
  expect(report).toContain("av1: supported=true hardware=true; hevc: supported=false");
  expect(report).toContain("- phase: connected");
  expect(report).toContain(
    "| 2560×1440 | 119.8 | 24000 | 4 | 1.2 | 0 | 0 | 1 | 2.1 | direct | AV1 |",
  );
  expect(report).toMatch(/Z peer connected ice connected; generation 1\n/);
  expect(report.endsWith("# nanoctl debug report\nhost body\n")).toBe(true);
});

test("keeps only the most recent events and samples", async () => {
  for (let i = 0; i < 400; i += 1) recordDebugEvent(`event ${i}`);
  for (let i = 0; i < 200; i += 1) recordMetrics({ ...metrics, fps: i });
  const report = await buildDebugReport({
    info: null,
    snapshot,
    socketStatus: "online",
    socketDetail: "",
    codecChoice: "auto",
    hostReport: "",
    decodeSupport: async () => ({ supported: true, powerEfficient: true }),
  });
  expect(report).not.toContain("event 99\n");
  expect(report).toContain("event 100\n");
  expect(report).toContain("event 399\n");
  expect(report.match(/^\| \d\d:/gm)?.length).toBe(120);
});

test("host report failures become readable text", async () => {
  const denied = (async () =>
    new Response("needs control\n", {
      status: 403,
    })) as unknown as typeof fetch;
  expect(await fetchHostReport(denied)).toBe("Host report unavailable (403): needs control");
  const offline = (async () => {
    throw new Error("network down");
  }) as unknown as typeof fetch;
  expect(await fetchHostReport(offline)).toBe("Host report unavailable: network down");
});

test("reports v4 measurements and the host encoder separately from RTP stats", async () => {
  recordMetrics(metrics);
  const report = await buildDebugReport({
    info: null,
    snapshot,
    socketStatus: "online",
    socketDetail: "",
    codecChoice: "auto",
    hostReport:
      '## Recent agent log\n\nINFO v4 platform encoder initialized backend="software-openh264"\n',
    decodeSupport: async () => ({ supported: true, powerEfficient: false }),
    v4Video: {
      resolution: "960×540",
      rttMs: 0,
      route: "direct",
      jitterMs: null,
      skippedFrames: 2,
      deliveredFrames: 3,
      skippedPercent: 40,
      decodeMs: 3,
      decodeSamples: 2,
    },
  });
  expect(report).toContain("decoded resolution: 960×540");
  expect(report).toContain("direct; RTT: 0.00 ms");
  expect(report).toContain("arrival jitter: unavailable");
  expect(report).toContain("skipped stream frames: 2 (40.00%)");
  expect(report).toContain("WebCodecs submit to output: 3.00 ms (2 samples)");
  expect(report).toContain("last reported encoder backend: software-openh264");
  expect(report).not.toContain("| packetsLost |");
});
