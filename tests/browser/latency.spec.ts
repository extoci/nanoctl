import { chromium, expect, test, type Page } from "@playwright/test";
import { mkdir, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { markerBits, readMarker, readMarkerRow, summarize } from "./latency-marker";
import { closePanel, openPanel } from "./panel";

const numInputs = 12;
const sampleMs = 10_000;

interface Frame {
  sequence: number;
  input: number;
  displayMs: number;
  presented: number;
}

declare global {
  interface Window {
    nanoctlMarker: { paintTimes: number[]; input: number };
    nanoctlProbe: { frames: Frame[]; inputTimes: number[] };
    nanoctlReadMarkerRow: typeof readMarkerRow;
  }
}

async function startMarker(page: Page) {
  await page.setContent(
    '<body style="margin:0;background:black;overflow:hidden"><canvas style="display:block;width:100vw;height:100vh"></canvas></body>',
  );
  await page.locator("canvas").click();
  await page.evaluate((encode) => {
    const bits = new Function(`return (${encode})`)() as typeof markerBits;
    const canvas = document.querySelector("canvas")!;
    canvas.width = 44 * 20;
    canvas.height = 4;
    const context = canvas.getContext("2d")!;
    context.fillStyle = "cyan";
    context.fillRect(0, 0, 80, 4);
    context.fillStyle = "magenta";
    context.fillRect(800, 0, 80, 4);
    const marker: Window["nanoctlMarker"] = { paintTimes: [], input: 0 };
    window.nanoctlMarker = marker;
    window.addEventListener("keydown", (event) => {
      if (event.isTrusted && event.code === "Space" && !event.repeat) {
        event.preventDefault();
        marker.input += 1;
      }
    });
    const render = () => {
      const sequence = marker.paintTimes.length;
      const bars = bits(sequence, marker.input);
      marker.paintTimes.push(performance.now());
      bars.forEach((bit, i) => {
        context.fillStyle = bit ? "white" : "black";
        context.fillRect((i + 4) * 20, 0, 20, 4);
      });
      requestAnimationFrame(render);
    };
    requestAnimationFrame(render);
  }, markerBits.toString());
}

async function startProbe(page: Page) {
  await page.evaluate(() => {
    const video = document.querySelector("video")!;
    const canvas = document.createElement("canvas");
    canvas.width = 640;
    canvas.height = 24;
    const context = canvas.getContext("2d", { willReadFrequently: true })!;
    const probe: Window["nanoctlProbe"] = { frames: [], inputTimes: [] };
    window.nanoctlProbe = probe;
    video.addEventListener(
      "keydown",
      (event) => {
        if (event.isTrusted && event.code === "Space" && !event.repeat)
          probe.inputTimes.push(event.timeStamp);
      },
      { capture: true },
    );

    const callback: VideoFrameRequestCallback = (_now, metadata) => {
      context.drawImage(video, 0, 0, canvas.width, canvas.height);
      const pixels = context.getImageData(0, 0, canvas.width, canvas.height).data;
      const stride = canvas.width * 4;
      let marker = window.nanoctlReadMarkerRow(pixels.subarray(stride * 12, stride * 13));
      for (let y = 1; !marker && y < canvas.height; y += 3) {
        marker = window.nanoctlReadMarkerRow(pixels.subarray(stride * y, stride * (y + 1)));
      }
      if (marker) {
        probe.frames.push({
          ...marker,
          // Chromium can reuse an expired display deadline for a newly submitted frame.
          displayMs: Math.max(metadata.expectedDisplayTime, metadata.presentationTime),
          presented: metadata.presentedFrames,
        });
      }
      video.requestVideoFrameCallback(callback);
    };
    video.requestVideoFrameCallback(callback);
  });
}

async function alignClock(page: Page) {
  let best = { offsetMs: 0, uncertaintyMs: Infinity };
  for (let i = 0; i < 10; i++) {
    const t0 = process.hrtime.bigint();
    const browserMs = await page.evaluate(() => performance.now());
    const t1 = process.hrtime.bigint();
    const uncertaintyMs = Number(t1 - t0) / 2e6;
    if (uncertaintyMs < best.uncertaintyMs) {
      best = { offsetMs: Number(t0) / 1e6 + uncertaintyMs - browserMs, uncertaintyMs };
    }
  }
  return best;
}

test("measures video and input through the real agent", async ({ page, browser }, testInfo) => {
  test.skip(process.env.NANOCTL_BROWSER_LATENCY !== "1", "Run on a dedicated desktop");
  test.setTimeout(90_000);
  const markerBrowser = await chromium.launch({
    headless: false,
    args: ["--kiosk"],
  });
  try {
    const host = await markerBrowser.newPage({ viewport: null });
    await startMarker(host);
    await page.goto("/");
    await expect(page.getByTestId("connection-state")).toHaveText("Live", { timeout: 30_000 });
    await openPanel(page);
    await expect(page.getByRole("button", { name: "Input on", exact: true })).toBeEnabled();
    await closePanel(page);
    await page.evaluate(`{
      const readMarker = ${readMarker.toString()};
      window.nanoctlReadMarkerRow = ${readMarkerRow.toString()};
    }`);
    await startProbe(page);
    await page.waitForFunction(() => window.nanoctlProbe.frames.length >= 30, undefined, {
      timeout: 15_000,
    });
    const hostClock = await alignClock(host);
    const viewerClock = await alignClock(page);
    const startMs = await page.evaluate(() => performance.now());
    const video = page.locator("video");
    await video.focus();
    // Playwright input goes to the headless viewer; OS input must go to the headed marker.
    await host.bringToFront();
    for (let i = 1; i <= numInputs; i++) {
      const nextMs = await page.evaluate((gap) => performance.now() + gap, 181 + ((i * 43) % 193));
      await page.waitForFunction((time) => performance.now() >= time, nextMs);
      await page.keyboard.press("Space");
      await page.waitForFunction(
        (input) => (window.nanoctlProbe.frames.at(-1)?.input ?? 0) >= input,
        i,
        { timeout: 5_000 },
      );
    }
    await page.waitForFunction((time) => performance.now() >= time, startMs + sampleMs);
    const { frames, inputTimes } = await page.evaluate(() => window.nanoctlProbe);
    const { paintTimes, input } = await host.evaluate(() => window.nanoctlMarker);
    const size = await video.evaluate((element: HTMLVideoElement) => ({
      width: element.videoWidth,
      height: element.videoHeight,
    }));
    expect(inputTimes).toHaveLength(numInputs);
    expect(input).toBe(numInputs);
    const steady = frames.filter((frame) => frame.displayMs >= startMs);
    const seen = new Set<number>();
    const unique = steady.filter(({ sequence }) => {
      if (seen.has(sequence)) return false;
      seen.add(sequence);
      return true;
    });
    expect(unique.length, "distinct source markers").toBeGreaterThanOrEqual(60);

    const clockDelta = viewerClock.offsetMs - hostClock.offsetMs;
    const uncertaintyMs = hostClock.uncertaintyMs + viewerClock.uncertaintyMs;
    const videoMs = unique.map(
      (frame) => frame.displayMs + clockDelta - paintTimes[frame.sequence],
    );
    expect(Math.min(...videoMs), "source time precedes presentation").toBeGreaterThanOrEqual(
      -uncertaintyMs,
    );
    const inputMs = inputTimes.map((time, i) => {
      const frame = steady.find(({ input }) => input === i + 1);
      expect(frame, `first visible response to input ${i + 1}`).toBeDefined();
      return frame!.displayMs - time;
    });
    expect(Math.min(...inputMs)).toBeGreaterThanOrEqual(0);
    const first = steady[0];
    const last = steady[steady.length - 1];
    const durationMs = last.displayMs - first.displayMs;
    const fps = ((last.presented - first.presented) * 1000) / durationMs;
    const report = {
      schemaVersion: 1,
      platform: process.platform,
      arch: process.arch,
      chromium: browser.version(),
      encoder: "software",
      quality: { maxWidth: 1920, maxHeight: 1080, maxFps: 60 },
      videoSize: size,
      durationMs,
      clockUncertaintyMs: uncertaintyMs,
      fps,
      video: { ...summarize(videoMs), samplesMs: videoMs },
      input: { ...summarize(inputMs), samplesMs: inputMs },
    };
    const row = (label: string, result: ReturnType<typeof summarize>) =>
      `| ${label} | ${result.count} | ${result.mean.toFixed(1)} | ${result.p50.toFixed(1)} | ${result.p95.toFixed(1)} | ${result.p99.toFixed(1)} | ${result.max.toFixed(1)} |`;
    const markdown = [
      `### nanoctl latency — ${report.platform}/${report.arch}`,
      "",
      `Chromium ${report.chromium}, ${size.width}×${size.height} at ${fps.toFixed(1)} presented fps (software H.264, loopback). Display estimates cannot precede compositor submission. Clock alignment uncertainty: ±${uncertaintyMs.toFixed(2)} ms.`,
      "",
      "| Metric | Samples | Mean (ms) | p50 (ms) | p95 (ms) | p99 (ms) | Max (ms) |",
      "| --- | ---: | ---: | ---: | ---: | ---: | ---: |",
      row("Video (host draw → viewer presentation)", report.video),
      row("Input (viewer keydown → response presentation)", report.input),
      "",
    ].join("\n");
    const output = testInfo.outputPath();
    await mkdir(output, { recursive: true });
    await Promise.all([
      writeFile(join(output, "latency.json"), JSON.stringify(report, null, 2) + "\n"),
      writeFile(join(output, "latency.md"), markdown),
    ]);
    console.log(markdown);
  } finally {
    await markerBrowser.close();
  }
});
