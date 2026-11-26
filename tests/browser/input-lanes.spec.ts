import { expect, test } from "@playwright/test";
import { execFileSync } from "node:child_process";
import { readFile } from "node:fs/promises";
import { closePanel, openPanel } from "./panel";
import { injectDataChannelSpy, injectedWire } from "./v4-input";

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
  await page.addInitScript(injectDataChannelSpy);
  await page.addInitScript(() => {
    const pad = {
      axes: [0, 0, 0, 0],
      buttons: Array.from({ length: 17 }, () => ({ pressed: false, value: 0 })),
      connected: true,
      id: "Xbox Wireless Controller",
      index: 0,
      mapping: "standard",
    };
    Object.defineProperty(navigator, "getGamepads", { value: () => [pad] });
    const state = { gamepad: 0 };
    Object.assign(window, { state });
    const frame = () => {
      pad.axes[0] = ((state.gamepad % 200) - 100) / 100;
      state.gamepad += 1;
      requestAnimationFrame(frame);
    };
    requestAnimationFrame(frame);
  });
  await page.goto("/");
  await openPanel(page);
  await expect(page.getByTestId("connection-state")).toHaveText("Live");
  // A held, drifting analog stick produces latest-state snapshots on the same
  // unordered v4 motion lane as pointer movement.
  await page.getByRole("button", { name: "Gamepad off" }).click();
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
  const sentBefore = (await injectedWire(page)).filter(
    (message) => message.channel === "nanoctl.motion.v4" && message.binary?.[1] === 0x08,
  ).length;
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
    (await injectedWire(page)).filter(
      (message) => message.channel === "nanoctl.motion.v4" && message.binary?.[1] === 0x08,
    ).length - sentBefore;
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
