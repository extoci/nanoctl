import { test, expect, type Page } from "@playwright/test";
import type { ControlMessage } from "../../packages/protocol/src/index";
import { mkdir } from "node:fs/promises";
import { join } from "node:path";
import { closePanel, openPanel } from "./panel";

type SpyWindow = Window & { injected: string[] };
async function controls(page: Page): Promise<ControlMessage[]> {
  return page.evaluate(() =>
    (window as unknown as SpyWindow).injected
      .map((data) => JSON.parse(data))
      .filter(
        (message) =>
          message.type === "pointer" || message.type === "key" || message.type === "text",
      ),
  );
}
async function clearControls(page: Page) {
  await page.evaluate(() => {
    (window as unknown as SpyWindow).injected = [];
  });
}
async function screenshot(page: Page, name: string) {
  const directory = process.env.NANOCTL_BROWSER_EVIDENCE_DIR;
  if (!directory) return;
  await mkdir(directory, { recursive: true });
  await page.screenshot({ path: join(directory, name) });
}

test("mobile layout, reachable touch targets, keyboard focus and persisted mode", async ({
  page,
}) => {
  if (process.env.NANOCTL_BROWSER_MEDIA !== "1") {
    // RTC-only hosts advertise view-only; exercise the keyboard UI without native injection.
    await page.routeWebSocket("**/v1/connect", (socket) => {
      const server = socket.connectToServer();
      server.onMessage((data) => {
        const message = JSON.parse(String(data));
        if (message.type === "hello") message.host.remoteInput = true;
        socket.send(JSON.stringify(message));
      });
    });
  }
  await page.goto("/");
  await openPanel(page);
  await expect(page.getByRole("button", { name: "Input on", exact: true })).toBeEnabled();
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(
    true,
  );
  const buttons = page.locator(".session-panel button");
  for (const button of await buttons.all()) {
    await button.scrollIntoViewIfNeeded();
    const box = await button.boundingBox();
    expect(box?.height).toBeGreaterThanOrEqual(44);
    expect(box!.x).toBeGreaterThanOrEqual(0);
    expect(box!.x + box!.width).toBeLessThanOrEqual(
      await page.evaluate(() => window.innerWidth + 1),
    );
  }
  const keyboard = page.getByRole("button", { name: "Keyboard", exact: true });
  await keyboard.click();
  await expect(page.getByRole("textbox", { name: "Remote keyboard" })).toBeFocused();
  await expect(page.getByRole("toolbar", { name: "Remote special keys" })).toBeVisible();
  await page.getByRole("button", { name: "Ctrl", exact: true }).click();
  await expect(page.getByRole("button", { name: "Ctrl", exact: true })).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  await expect(page.getByRole("textbox", { name: "Remote keyboard" })).toBeFocused();
  await screenshot(page, "pixel7-keyboard-ctrl.png");
  await page.getByRole("button", { name: "Hide special keys" }).click();
  await keyboard.click();
  await openPanel(page);
  await page.getByRole("button", { name: "Trackpad", exact: true }).click();
  await expect(page.getByRole("button", { name: "Direct", exact: true })).toBeVisible();
  await page.reload();
  await expect(page.getByRole("button", { name: "Direct", exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Direct", exact: true }).click();
  await page.reload();
  await expect(page.getByRole("button", { name: "Trackpad", exact: true })).toBeVisible();
  await expect(page.getByRole("button", { name: "Input on", exact: true })).toBeEnabled();
  if (process.env.NANOCTL_BROWSER_MEDIA === "1") {
    await expect(page.getByTestId("connection-state")).toHaveText("Live", {
      timeout: 30_000,
    });
  }
  await page.setViewportSize({ width: 844, height: 390 });
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(
    true,
  );
  await screenshot(page, "iphone-landscape-connected.png");
});

test("mobile live touch, scroll and keyboard control respect the input toggle", async ({
  page,
  context,
}) => {
  test.skip(process.env.NANOCTL_BROWSER_MEDIA !== "1", "Requires live video");
  await page.addInitScript(() => {
    const send = RTCDataChannel.prototype.send;
    (window as unknown as SpyWindow).injected = [];
    RTCDataChannel.prototype.send = function (data: unknown) {
      (window as unknown as SpyWindow).injected.push(String(data));
      return Reflect.apply(send, this, [data]);
    };
  });
  await page.goto("/");
  await expect(page.getByTestId("connection-state")).toHaveText("Live", {
    timeout: 30_000,
  });
  await expect(page.locator(".touch-cursor")).toBeVisible();
  const box = (await page.locator("video").boundingBox())!;
  const x = box.x + box.width / 2;
  const y = box.y + box.height / 2;
  const cdp = await context.newCDPSession(page);
  const touch = (
    type: "touchStart" | "touchMove" | "touchEnd",
    points: { x: number; y: number; id: number }[],
  ) => cdp.send("Input.dispatchTouchEvent", { type, touchPoints: points });
  const tap = async () => {
    await touch("touchStart", [{ x, y, id: 1 }]);
    await touch("touchEnd", []);
  };
  const scroll = async () => {
    await touch("touchStart", [
      { x: x - 50, y, id: 1 },
      { x: x + 50, y, id: 2 },
    ]);
    await touch("touchMove", [
      { x: x - 50, y: y + 40, id: 1 },
      { x: x + 50, y: y + 40, id: 2 },
    ]);
    await touch("touchEnd", []);
  };
  await clearControls(page);
  await tap();
  await expect
    .poll(async () =>
      (await controls(page))
        .filter((m) => m.type === "pointer" && (m.action === "down" || m.action === "up"))
        .map((m) => m.type === "pointer" && m.action),
    )
    .toEqual(["down", "up"]);
  await screenshot(page, "pixel7-live-cursor.png");
  await clearControls(page);
  await scroll();
  await expect
    .poll(async () =>
      (await controls(page)).some(
        (m) => m.type === "pointer" && m.action === "wheel" && Math.abs(m.deltaY ?? 0) >= 100,
      ),
    )
    .toBe(true);
  await openPanel(page);
  await page.getByRole("button", { name: "Keyboard", exact: true }).click();
  const field = page.getByRole("textbox", { name: "Remote keyboard" });
  await expect(field).toBeFocused();
  await clearControls(page);
  const text = "hi" + "phone ready ".repeat(3) + "ok";
  await page.keyboard.insertText(text);
  await page.keyboard.press("Enter");
  await expect
    .poll(async () => (await controls(page)).filter((m) => m.type === "key" || m.type === "text"), {
      timeout: 15_000,
    })
    .toEqual([
      { type: "text", text },
      {
        type: "key",
        key: "Enter",
        code: "Enter",
        action: "down",
        modifiers: 0,
        repeat: false,
      },
      {
        type: "key",
        key: "Enter",
        code: "Enter",
        action: "up",
        modifiers: 0,
        repeat: false,
      },
    ]);
  await page.getByRole("button", { name: "Ctrl", exact: true }).click();
  await expect(field).toBeFocused();
  await screenshot(page, "pixel7-live-keyboard-ctrl.png");
  await page.setViewportSize({ width: 844, height: 390 });
  await screenshot(page, "iphone-landscape-live-keyboard-ctrl.png");
  await page.getByRole("button", { name: "Hide special keys" }).click();
  await openPanel(page);
  await page.getByRole("button", { name: "Input on", exact: true }).click();
  await expect(page.getByRole("button", { name: "Keyboard", exact: true })).toBeDisabled();
  await closePanel(page);
  await page.setViewportSize({ width: 393, height: 851 });
  await clearControls(page);
  await tap();
  await scroll();
  await field.dispatchEvent("beforeinput", {
    inputType: "insertText",
    data: "hi",
    bubbles: true,
    cancelable: true,
  });
  await field.dispatchEvent("keydown", {
    key: "Enter",
    code: "Enter",
    bubbles: true,
    cancelable: true,
  });
  expect(await controls(page)).toEqual([]);
  await cdp.detach();
});
