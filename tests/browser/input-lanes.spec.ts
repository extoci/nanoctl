import { expect, test } from "@playwright/test";
import { execFileSync } from "node:child_process";
import { readFile } from "node:fs/promises";
import { closePanel, openPanel } from "./panel";

const moves = 40;

const hasXdotool = (() => {
  try {
    execFileSync("xdotool", ["version"], { stdio: "ignore" });
    return true;
  } catch {
    return false;
  }
})();

function hostCursor(): { x: number; y: number } {
  const [, x, y] = /x:(\d+) y:(\d+)/.exec(
    execFileSync("xdotool", ["getmouselocation"], { encoding: "utf8" }),
  )!;
  return { x: Number(x), y: Number(y) };
}

test("pointer motion and active gamepad snapshots never clobber each other", async ({ page }) => {
  const reports = process.env.NANOCTL_DEV_GAMEPAD_REPORTS;
  test.skip(
    !reports || process.platform !== "linux" || !process.env.DISPLAY || !hasXdotool,
    "Requires the media agent with a mock virtual gamepad, Linux native input and xdotool",
  );
  await page.addInitScript(() => {
    const state = { pointer: null as RTCDataChannel | null, gamepad: 0 };
    Object.assign(window, { state });
    const create = RTCPeerConnection.prototype.createDataChannel;
    RTCPeerConnection.prototype.createDataChannel = function (...args) {
      const channel = Reflect.apply(create, this, args) as RTCDataChannel;
      if (args[0] === "nanoctl.pointer.v1") state.pointer = channel;
      return channel;
    };
  });
  await page.goto("/");
  await openPanel(page);
  await expect(page.getByTestId("connection-state")).toHaveText("Live");
  // A held, drifting analog stick: one changed snapshot per frame on the pointer lane, sent from
  // requestAnimationFrame like the viewer and iOS forwarders, after that frame's pointer move.
  await page.evaluate(() => {
    const state = (window as unknown as { state: { pointer: RTCDataChannel; gamepad: number } })
      .state;
    const frame = () => {
      const axis = ((state.gamepad % 200) - 100) * 300;
      state.pointer.send(JSON.stringify({ type: "gamepad", pads: [[0, 0, 0, 0, axis, 0, 0, 0]] }));
      state.gamepad += 1;
      requestAnimationFrame(frame);
    };
    requestAnimationFrame(frame);
  });
  await expect.poll(() => readFile(reports!, "utf8").catch(() => "")).toContain("0 attached");
  await closePanel(page);
  const [width, height] = execFileSync("xdotool", ["getdisplaygeometry"], {
    encoding: "utf8",
  })
    .trim()
    .split(" ")
    .map(Number);
  const geometry = await page.evaluate(() => {
    const video = document.querySelector("video")!;
    const rect = video.getBoundingClientRect();
    const scale = Math.min(rect.width / video.videoWidth, rect.height / video.videoHeight);
    const w = video.videoWidth * scale;
    const h = video.videoHeight * scale;
    return {
      left: rect.left + (rect.width - w) / 2,
      top: rect.top + (rect.height - h) / 2,
      w,
      h,
    };
  });
  const before = (await readFile(reports!, "utf8")).split("\n").length;
  const sentBefore = await page.evaluate(
    () => (window as unknown as { state: { gamepad: number } }).state.gamepad,
  );
  const settle: number[] = [];
  let lost = 0;
  for (let index = 0; index < moves; index++) {
    const nx = 0.15 + 0.7 * (((index * 17) % moves) / moves);
    const ny = 0.15 + 0.7 * (((index * 29) % moves) / moves);
    const expected = {
      x: Math.round(nx * (width! - 1)),
      y: Math.round(ny * (height! - 1)),
    };
    const started = performance.now();
    await page.mouse.move(geometry.left + nx * geometry.w, geometry.top + ny * geometry.h);
    let reached = false;
    while (performance.now() - started < 250) {
      const cursor = hostCursor();
      if (Math.abs(cursor.x - expected.x) <= 4 && Math.abs(cursor.y - expected.y) <= 4) {
        reached = true;
        break;
      }
      await page.waitForTimeout(2);
    }
    if (reached) settle.push(performance.now() - started);
    else lost += 1;
    await page.waitForTimeout(30);
  }
  const sent =
    (await page.evaluate(
      () => (window as unknown as { state: { gamepad: number } }).state.gamepad,
    )) - sentBefore;
  const applied = (await readFile(reports!, "utf8"))
    .split("\n")
    .slice(before - 1)
    .filter((line) => /^0 \d/.test(line)).length;
  settle.sort((a, b) => a - b);
  console.log(
    JSON.stringify({
      moves,
      lostFinalPositions: lost,
      settleP50Ms: settle[Math.floor(settle.length / 2)]?.toFixed(1),
      settleP95Ms: settle[Math.floor(settle.length * 0.95)]?.toFixed(1),
      gamepadSent: sent,
      gamepadApplied: applied,
    }),
  );
  expect(lost).toBe(0);
  expect(applied).toBeGreaterThanOrEqual(sent * 0.95);
});
