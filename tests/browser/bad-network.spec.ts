import { chromium, expect, test, type Page } from "@playwright/test";
import { execFile } from "node:child_process";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { dirname } from "node:path";
import { promisify } from "node:util";
import {
  encodeVideoControlMessage,
  VIDEO_DELTA_CHANNEL_LABEL,
  VIDEO_REFERENCE_CHANNEL_LABEL,
} from "../../packages/protocol/src/index";
import { earlyMediaExits, longestGap, mostInOneSecond } from "./bad-network-evidence";
import { markerBits, readMarker, readMarkerRow } from "./latency-marker";

const run = promisify(execFile);
const durationMs = 60_000;
const maximumFreezeMs = 500;
const referenceBufferBytes = 512 * 1024;
const rssAllowanceKiB = 32 * 1024;
const reconnects = 20;

interface GateProbe {
  timeOrigin: number;
  frames: number[];
  keyframes: number[];
  keyframeArrivals: {
    timestampMs: number;
    captureTimeUs: number;
    streamSequence: number;
    lane: "delta" | "reference";
  }[];
  keyframeRequests: number[];
  deltaRepairRequests: { timestampMs: number; streamSequence: number; parts: number[] }[];
  startupFragments: {
    timestampMs: number;
    streamSequence: number;
    part: number;
    parts: number;
    lane: "delta" | "reference";
  }[];
  wireArrivals: number[];
  decodeSubmissions: number[];
  maxDecoderQueue: number;
  lastWireMs: number | null;
  wireBytes: number;
  presentedFrames: number;
}

declare global {
  interface Window {
    badNetworkProbe: GateProbe;
    badNetworkReadRow: typeof readMarkerRow;
  }
}

interface Netem {
  kind: string;
  handle: string;
  options: {
    limit: number;
    delay: { delay: number; jitter: number };
    "loss-random": { loss: number };
    reorder: { reorder: number };
    rate: { rate: number };
  };
  bytes: number;
  packets: number;
  drops: number;
  backlog: number;
  qlen: number;
}

async function readNetem(): Promise<Netem> {
  const { stdout } = await run("tc", ["-j", "-s", "-d", "qdisc", "show", "dev", "lo"]);
  const qdiscs = JSON.parse(stdout) as Netem[];
  const netem = qdiscs.find(({ kind, handle }) => kind === "netem" && handle === "10:");
  if (!netem) throw new Error("The isolated UDP impairment queue is missing");
  return netem;
}

async function openScene(host: Page) {
  await host.setContent(
    '<body style="margin:0;overflow:hidden;background:black"><canvas></canvas></body>',
  );
  await host.evaluate((encode) => {
    const bits = new Function(`return (${encode})`)() as typeof markerBits;
    const canvas = document.querySelector("canvas")!;
    canvas.width = innerWidth;
    canvas.height = innerHeight;
    const ctx = canvas.getContext("2d")!;
    const texture = document.createElement("canvas");
    texture.width = texture.height = 256;
    const tile = texture.getContext("2d")!;
    const image = tile.createImageData(256, 256);
    let seed = 0xc01dcafe;
    for (let i = 0; i < image.data.length; i += 4) {
      seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0;
      image.data[i] = 32 + ((seed >>> 24) & 127);
      image.data[i + 1] = 48 + ((seed >>> 16) & 127);
      image.data[i + 2] = 64 + ((seed >>> 8) & 127);
      image.data[i + 3] = 255;
    }
    tile.putImageData(image, 0, 0);
    const pattern = ctx.createPattern(texture, "repeat")!;
    let sequence = 0;
    function paint() {
      ctx.save();
      const x = sequence % 256;
      ctx.translate(-x, -((sequence * 2) % 256));
      ctx.fillStyle = pattern;
      ctx.fillRect(0, 0, canvas.width + 256, canvas.height + 256);
      ctx.restore();

      const y = Math.floor(canvas.height / 2) - 64;
      ctx.fillStyle = "cyan";
      ctx.fillRect(0, y, 80, 128);
      ctx.fillStyle = "magenta";
      ctx.fillRect(800, y, 80, 128);
      bits(sequence & 0xffff, 0).forEach((bit, index) => {
        ctx.fillStyle = bit ? "white" : "black";
        ctx.fillRect(80 + index * 20, y, 20, 128);
      });
      sequence += 1;
      requestAnimationFrame(paint);
    }
    requestAnimationFrame(paint);
  }, markerBits.toString());
  await host.bringToFront();
}

async function monitorBrowser(page: Page) {
  await page.addInitScript(
    ({ delta, reference, keyframeRequest, deltaRepairHeader }) => {
      const probe: GateProbe = {
        timeOrigin: performance.timeOrigin,
        frames: [],
        keyframes: [],
        keyframeArrivals: [],
        keyframeRequests: [],
        deltaRepairRequests: [],
        startupFragments: [],
        wireArrivals: [],
        decodeSubmissions: [],
        maxDecoderQueue: 0,
        lastWireMs: null,
        wireBytes: 0,
        presentedFrames: 0,
      };
      window.badNetworkProbe = probe;
      if (typeof VideoDecoder === "function") {
        const decode = VideoDecoder.prototype.decode;
        VideoDecoder.prototype.decode = function (chunk) {
          decode.call(this, chunk);
          probe.maxDecoderQueue = Math.max(probe.maxDecoderQueue, this.decodeQueueSize);
          probe.decodeSubmissions.push(performance.now());
        };
      }
      const send = RTCDataChannel.prototype.send;
      RTCDataChannel.prototype.send = function (data) {
        Reflect.apply(send, this, [data]);
        if (this.label !== reference || !(data instanceof Uint8Array)) return;
        if (
          data.length === keyframeRequest.length &&
          keyframeRequest.every((byte, index) => data[index] === byte)
        ) {
          probe.keyframeRequests.push(performance.now());
        } else if (
          data.length >= 6 &&
          deltaRepairHeader.every((byte, index) => data[index] === byte)
        ) {
          const wire = new DataView(data.buffer, data.byteOffset, data.byteLength);
          probe.deltaRepairRequests.push({
            timestampMs: performance.now(),
            streamSequence: wire.getUint32(2, true),
            parts: Array.from({ length: (data.length - 6) / 2 }, (_, index) =>
              wire.getUint16(6 + index * 2, true),
            ),
          });
        }
      };
      const videoPeers = new WeakMap<
        RTCPeerConnection,
        { startup: boolean; keyframes: Set<number> }
      >();
      let startupPeerAssigned = false;
      const create = RTCPeerConnection.prototype.createDataChannel;
      RTCPeerConnection.prototype.createDataChannel = function (...args) {
        const channel = Reflect.apply(create, this, args) as RTCDataChannel;
        if (channel.label === delta || channel.label === reference) {
          const lane = channel.label === delta ? "delta" : "reference";
          let peer = videoPeers.get(this);
          if (!peer) {
            peer = { startup: !startupPeerAssigned, keyframes: new Set<number>() };
            videoPeers.set(this, peer);
            startupPeerAssigned = true;
          }
          const { startup, keyframes: seen } = peer;
          channel.addEventListener("message", ({ data }) => {
            if (!(data instanceof ArrayBuffer) || data.byteLength < 20) return;
            const now = performance.now();
            probe.lastWireMs = now;
            probe.wireBytes += data.byteLength;
            const lastSample = probe.wireArrivals.at(-1);
            if (lastSample === undefined || now - lastSample >= 10) probe.wireArrivals.push(now);
            const wire = new DataView(data);
            const sequence = wire.getUint32(2, true);
            const part = wire.getUint16(6, true);
            if (startup && sequence <= 16 && probe.startupFragments.length < 2_048) {
              probe.startupFragments.push({
                timestampMs: now,
                streamSequence: sequence,
                part,
                parts: wire.getUint16(8, true),
                lane,
              });
            }
            if ((wire.getUint8(11) & 1) !== 0 && part === 0) {
              if (!seen.has(sequence)) {
                seen.add(sequence);
                probe.keyframes.push(now);
                probe.keyframeArrivals.push({
                  timestampMs: now,
                  captureTimeUs: Number(wire.getBigUint64(12, true)),
                  streamSequence: sequence,
                  lane,
                });
              }
            }
          });
        }
        return channel;
      };
    },
    {
      delta: VIDEO_DELTA_CHANNEL_LABEL,
      reference: VIDEO_REFERENCE_CHANNEL_LABEL,
      keyframeRequest: Array.from(encodeVideoControlMessage({ kind: "keyframeRequest" })),
      deltaRepairHeader: Array.from(
        encodeVideoControlMessage({ kind: "deltaRepair", streamSequence: 0 }).slice(0, 2),
      ),
    },
  );
}

async function monitorFrames(page: Page) {
  await page.evaluate(`{
    const readMarker = ${readMarker.toString()};
    window.badNetworkReadRow = ${readMarkerRow.toString()};
  }`);
  await page.evaluate(() => {
    const video = document.querySelector("video")!;
    const canvas = document.createElement("canvas");
    canvas.width = 640;
    canvas.height = 36;
    const ctx = canvas.getContext("2d", { willReadFrequently: true })!;
    let lastSequence: number | undefined;
    const probe = window.badNetworkProbe;
    const present: VideoFrameRequestCallback = () => {
      if (video.videoWidth > 0 && video.videoHeight > 0) {
        probe.presentedFrames += 1;
        ctx.drawImage(video, 0, 0, canvas.width, canvas.height);
        const pixels = ctx.getImageData(0, 0, canvas.width, canvas.height).data;
        const stride = canvas.width * 4;
        let marker = window.badNetworkReadRow(pixels.subarray(stride * 18, stride * 19));
        for (let y = 1; !marker && y < canvas.height; y += 3) {
          marker = window.badNetworkReadRow(pixels.subarray(stride * y, stride * (y + 1)));
        }
        if (marker !== undefined && marker.sequence !== lastSequence) {
          lastSequence = marker.sequence;
          probe.frames.push(performance.now());
        }
      }
      video.requestVideoFrameCallback(present);
    };
    video.requestVideoFrameCallback(present);
  });
}

async function hostRss(pid: number) {
  const status = await readFile(`/proc/${pid}/status`, "utf8");
  const rss = /^VmRSS:\s+(\d+) kB$/m.exec(status)?.[1];
  if (!rss) throw new Error("Cannot read the live media agent RSS");
  return Number(rss);
}

test("v4 real media survives 60 seconds and reconnect churn under packet impairment", async ({
  page,
  request,
}, testInfo) => {
  test.skip(
    process.env.NANOCTL_BROWSER_BAD_NETWORK !== "1",
    "Run through scripts/v4-bad-network.sh",
  );
  test.setTimeout(270_000);
  const pidFile = process.env.NANOCTL_BROWSER_AGENT_PID_FILE;
  if (!pidFile) throw new Error("The media-agent PID file is missing");
  const launcher = (await readFile(pidFile, "utf8")).trim();
  const { stdout } = await run("ps", ["-o", "pid=", "--ppid", launcher]);
  const children = stdout.trim().split(/\s+/).map(Number);
  if (children.length !== 1 || !Number.isInteger(children[0]) || children[0]! <= 0)
    throw new Error("Expected one prebuilt media-agent process");
  const pid = children[0]!;

  const before = await readNetem();
  expect(before.options).toMatchObject({
    limit: 128,
    delay: { delay: 0.025, jitter: 0.008 },
    "loss-random": { loss: 0.05 },
    reorder: { reorder: 0.25 },
    rate: { rate: 375_000 },
  });

  const desktop = await chromium.launch({ headless: false, args: ["--kiosk"] });
  const evidence: Record<string, unknown> = { profile: before.options };
  try {
    await openScene(await desktop.newPage({ viewport: { width: 1280, height: 720 } }));
    await monitorBrowser(page);
    await page.goto("/");
    await monitorFrames(page);
    const attachStart = await page.evaluate(() => performance.now());
    const firstFrame = await page
      .waitForFunction(() => window.badNetworkProbe.frames.length > 0, undefined, {
        timeout: 10_000,
      })
      .then(() => page.evaluate(() => window.badNetworkProbe.frames[0]!))
      .catch(() => null);
    const browserStart = await page.evaluate(() => performance.now());
    const rss: number[] = [];
    const queue: { qlen: number; backlog: number }[] = [];
    const sampleStarted = performance.now();
    while (performance.now() - sampleStarted < durationMs) {
      await new Promise<void>((resolve) => setTimeout(resolve, 1_000));
      rss.push(await hostRss(pid));
      const { qlen, backlog } = await readNetem();
      queue.push({ qlen, backlog });
    }
    const probe = await page.evaluate(() => window.badNetworkProbe);
    const sampleEnd = browserStart + durationMs;
    const keyframes = probe.keyframes.filter(
      (stamp) => stamp >= browserStart && stamp <= sampleEnd,
    );
    const gapMs = longestGap(probe.frames, browserStart, sampleEnd);
    const peakIdr = mostInOneSecond(keyframes);
    const after = await readNetem();
    const rssGrowthKiB = Math.max(...rss.slice(10)) - rss[9]!;
    Object.assign(evidence, {
      browserTimeOriginMs: probe.timeOrigin,
      frames: probe.frames,
      keyframes,
      keyframeArrivals: probe.keyframeArrivals,
      keyframeRequests: probe.keyframeRequests,
      deltaRepairRequests: probe.deltaRepairRequests,
      startupFragments: probe.startupFragments,
      wireArrivals: probe.wireArrivals,
      decodeSubmissions: probe.decodeSubmissions,
      maxDecoderQueue: probe.maxDecoderQueue,
      sampleEnd,
      browserStart,
      firstFrameMs: firstFrame === null ? null : Math.max(0, firstFrame - attachStart),
      gapMs,
      wireGapMs: longestGap(probe.wireArrivals, browserStart, sampleEnd),
      decodeSubmitGapMs: longestGap(probe.decodeSubmissions, browserStart, sampleEnd),
      peakIdr,
      senderSilenceMs:
        probe.lastWireMs === null ? durationMs : Math.max(0, sampleEnd - probe.lastWireMs),
      receivedMediaBytes: probe.wireBytes,
      presentedFrames: probe.presentedFrames,
      netemBytes: after.bytes - before.bytes,
      netemPackets: after.packets - before.packets,
      netemDrops: after.drops - before.drops,
      rss,
      rssGrowthKiB,
      queue,
    });

    expect
      .soft(after.packets - before.packets, "real UDP traffic in the impairment queue")
      .toBeGreaterThan(100);
    expect
      .soft(after.bytes - before.bytes, "media bytes traversed the impairment queue")
      .toBeGreaterThanOrEqual(probe.wireBytes);
    expect.soft(after.drops - before.drops, "impaired packet count").toBeGreaterThan(0);
    expect.soft(firstFrame, "first decoded desktop marker within the attach budget").not.toBeNull();
    expect
      .soft(gapMs, "longest freeze in newly presented desktop content")
      .toBeLessThanOrEqual(maximumFreezeMs);
    expect.soft(peakIdr, "peak keyframes in a rolling second").toBeLessThanOrEqual(4);
    expect
      .soft(keyframes.length, "sustained keyframe rate")
      .toBeLessThanOrEqual((2 * durationMs) / 1000);
    expect
      .soft(rssGrowthKiB, "post-warmup host RSS growth in KiB")
      .toBeLessThanOrEqual(rssAllowanceKiB);
    expect
      .soft(Math.max(...queue.map(({ qlen }) => qlen)), "netem packet queue")
      .toBeLessThanOrEqual(before.options.limit);
    await expect(page.getByTestId("connection-state")).toHaveText("Live", { timeout: 2_000 });
    const activeReport = await request.get("/v1/debug");
    const activeLog = await activeReport.text();
    const activeLines = activeLog.split("\n");
    evidence.sctpTimeouts = activeLines.filter((line) => line.includes("T3-rtx timed out"));
    const earlyStops = earlyMediaExits(activeLines, probe.timeOrigin + browserStart);
    evidence.earlyStops = earlyStops;
    expect.soft(activeReport.ok(), "host debug report during the measured session").toBe(true);
    expect.soft(earlyStops, "host pipeline exits before the viewer disconnects").toEqual([]);

    const endSession = async () => {
      await page.getByRole("button", { name: "Session options" }).click();
      await page.getByRole("button", { name: "Disconnect", exact: true }).click();
      await expect(page.getByRole("button", { name: "Reconnect", exact: true })).toBeVisible();
    };
    await endSession();
    let maxBuffered = NaN;
    await expect
      .configure({ soft: true })
      .poll(
        async () => {
          const report = await request.get("/v1/debug");
          const log = await report.text();
          const lines = log.split("\n");
          evidence.debugStatus = report.status();
          evidence.mediaEvents = lines
            .filter((line) => line.includes("v4") || line.includes("Viewer disconnected"))
            .slice(-15);
          maxBuffered = Number(
            lines
              .findLast((line) => line.includes("v4 media pipeline stopped"))
              ?.match(/max_buffered=(\d+)/)?.[1],
          );
          return Number.isFinite(maxBuffered);
        },
        {
          timeout: 5_000,
          message: "host reports the measured v4 pipeline's buffer watermark on teardown",
        },
      )
      .toBe(true);
    evidence.maxBufferedBytes = maxBuffered;
    expect
      .soft(maxBuffered, "host media lane buffer watermark")
      .toBeLessThanOrEqual(referenceBufferBytes);

    const readReconnectCounters = () =>
      page.evaluate(() => {
        const probe = window.badNetworkProbe;
        return {
          wireBytes: probe.wireBytes,
          keyframes: probe.keyframes.length,
          decodeSubmissions: probe.decodeSubmissions.length,
        };
      });
    for (let cycle = 1; cycle <= reconnects; cycle += 1) {
      const beforeReconnect = await readReconnectCounters();
      await page.getByRole("button", { name: "Reconnect", exact: true }).click();
      try {
        await expect(page.getByTestId("connection-state"), `impaired reattach ${cycle}`).toHaveText(
          "Live",
          { timeout: 8_000 },
        );
      } catch (error) {
        const [counters, host] = await Promise.allSettled([
          readReconnectCounters(),
          request.get("/v1/debug", { timeout: 2_000 }).then((report) => report.text()),
        ]);
        const hostLines = host.status === "fulfilled" ? host.value.split("\n") : undefined;
        evidence.failedReconnect = {
          cycle,
          before: beforeReconnect,
          after: counters.status === "fulfilled" ? counters.value : undefined,
          mediaEvents: hostLines
            ?.filter((line) => line.includes("v4") || line.includes("Viewer disconnected"))
            .slice(-15),
          sctpTimeouts: hostLines?.filter((line) => line.includes("T3-rtx timed out")).slice(-25),
        };
        throw error;
      }
      await endSession();
      evidence.reconnectsCompleted = cycle;
    }
  } finally {
    const path = testInfo.outputPath("bad-network.json");
    await mkdir(dirname(path), { recursive: true });
    await writeFile(path, JSON.stringify(evidence, null, 2));
    await testInfo.attach("bad-network", { path, contentType: "application/json" });
    await desktop.close();
  }
});
