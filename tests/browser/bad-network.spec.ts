import { chromium, expect, test } from "@playwright/test";
import { mkdir, writeFile } from "node:fs/promises";
import { join } from "node:path";

interface FrameEvent {
  time: number;
  presented: number;
}

interface MotionPhase {
  start: number;
  end: number;
}

const phases: MotionPhase[] = [
  { start: 0, end: 15_000 },
  { start: 25_000, end: 40_000 },
  { start: 50_000, end: 60_000 },
];

function phaseStats(frames: FrameEvent[], phase: MotionPhase) {
  const inside = frames.filter((frame) => frame.time >= phase.start && frame.time < phase.end);
  let maxGapMs = inside.length === 0 ? phase.end - phase.start : inside[0].time - phase.start;
  for (let index = 1; index < inside.length; index += 1) {
    maxGapMs = Math.max(maxGapMs, inside[index].time - inside[index - 1].time);
  }
  return {
    frames: inside.length,
    fps: inside.length / ((phase.end - phase.start) / 1000),
    maxGapMs,
  };
}

test("video survives 5% loss, 20% reorder, jitter, idle transitions, and 3 Mbps", async ({
  page,
}, testInfo) => {
  test.skip(process.env.NANOCTL_BROWSER_BAD_NETWORK !== "1", "Requires bad-network harness");
  test.setTimeout(90_000);

  const markerBrowser = await chromium.launch({ headless: false, args: ["--kiosk"] });
  try {
    const marker = await markerBrowser.newPage();
    await marker.setContent(
      '<body style="margin:0;background:black;overflow:hidden"><canvas style="display:block;width:100vw;height:100vh"></canvas></body>',
    );
    await marker.evaluate(() => {
      const canvas = document.querySelector("canvas")!;
      canvas.width = 640;
      canvas.height = 360;
      const context = canvas.getContext("2d")!;
      const state = { enabled: true, paints: 0 };
      (window as unknown as { badNetworkMotion: typeof state }).badNetworkMotion = state;
      const render = () => {
        if (state.enabled) {
          state.paints += 1;
          context.fillStyle = "black";
          context.fillRect(0, 0, canvas.width, canvas.height);
          for (let index = 0; index < 32; index += 1) {
            const x = (state.paints * (3 + (index % 5)) + index * 173) % canvas.width;
            const y = (state.paints * (2 + (index % 3)) + index * 97) % canvas.height;
            context.fillStyle = `hsl(${(state.paints * 7 + index * 31) % 360} 90% 55%)`;
            context.fillRect(x, y, 90 + (index % 4) * 30, 34 + (index % 3) * 24);
          }
        }
        requestAnimationFrame(render);
      };
      requestAnimationFrame(render);
    });

    await page.goto("/");
    await expect(page.getByTestId("connection-state")).toHaveText("Live", { timeout: 30_000 });
    await expect
      .poll(() => page.locator("video").evaluate((video) => (video as HTMLVideoElement).videoWidth))
      .toBeGreaterThan(0);
    const zero = await page.evaluate(() => {
      const video = document.querySelector("video")!;
      const probe = { frames: [] as FrameEvent[], zero: performance.now() };
      (window as unknown as { badNetworkProbe: typeof probe }).badNetworkProbe = probe;
      const callback: VideoFrameRequestCallback = (now, metadata) => {
        probe.frames.push({ time: now - probe.zero, presented: metadata.presentedFrames });
        video.requestVideoFrameCallback(callback);
      };
      video.requestVideoFrameCallback(callback);
      return probe.zero;
    });

    const setMotion = async (enabled: boolean) => {
      await marker.evaluate((enabled) => {
        (window as unknown as { badNetworkMotion: { enabled: boolean } }).badNetworkMotion.enabled =
          enabled;
      }, enabled);
    };
    const waitUntil = async (ms: number) => {
      await page.waitForFunction((deadline) => performance.now() >= deadline, zero + ms);
    };
    await setMotion(true);
    await waitUntil(15_000);
    await setMotion(false);
    await waitUntil(25_000);
    await setMotion(true);
    await waitUntil(40_000);
    await setMotion(false);
    await waitUntil(50_000);
    await setMotion(true);
    await waitUntil(60_000);

    await expect(page.getByTestId("connection-state")).toHaveText("Live");
    const result = await page.evaluate(() => {
      const probe = (window as unknown as { badNetworkProbe: { frames: FrameEvent[] } })
        .badNetworkProbe;
      return {
        frames: probe.frames,
        dcv: { ...(window as unknown as { __dcv?: Record<string, number> }).__dcv },
        videoWidth: (document.querySelector("video") as HTMLVideoElement).videoWidth,
      };
    });
    const motion = phases.map((phase) => ({ ...phase, ...phaseStats(result.frames, phase) }));
    const sourcePaints = await marker.evaluate(
      () => (window as unknown as { badNetworkMotion: { paints: number } }).badNetworkMotion.paints,
    );
    const totals = {
      presentedFrames: result.frames.at(-1)?.presented ?? 0,
      sourcePaints,
      keyframes: result.dcv.keys ?? 0,
      keyframeRequests: result.dcv.kf ?? 0,
      gaps: result.dcv.gaps ?? 0,
      decoderErrors: result.dcv.decoderErrors ?? 0,
      decoded: result.dcv.decoded ?? 0,
    };
    const report = {
      schemaVersion: 1,
      durationMs: 60_000,
      impairment: {
        lossPercent: 5,
        reorderPercent: 20,
        jitterMsMax: 25,
        bandwidthKbps: 3000,
        idleTransitions: 2,
      },
      motion,
      totals,
    };
    const output = testInfo.outputPath();
    await mkdir(output, { recursive: true });
    await writeFile(join(output, "bad-network.json"), JSON.stringify(report, null, 2) + "\n");
    console.log(
      `bad-network: fps=${motion.map((phase) => phase.fps.toFixed(1)).join("/")}, max-gap-ms=${motion
        .map((phase) => phase.maxGapMs.toFixed(0))
        .join("/")}, keyframes=${totals.keyframes}, decoder-errors=${totals.decoderErrors}`,
    );

    expect(result.videoWidth).toBeGreaterThan(0);
    expect(sourcePaints, "source must actually animate during the test").toBeGreaterThan(1000);
    expect(totals.decoded).toBeGreaterThan(0);
    expect(totals.decoderErrors).toBeLessThan(3);
    expect(totals.keyframes).toBeLessThanOrEqual(90);
    for (const phase of motion) {
      expect(phase.fps, `motion phase ${phase.start}-${phase.end} fps`).toBeGreaterThanOrEqual(5);
      expect(
        phase.maxGapMs,
        `motion phase ${phase.start}-${phase.end} maximum presentation gap`,
      ).toBeLessThanOrEqual(1500);
    }
  } finally {
    await markerBrowser.close();
  }
});
