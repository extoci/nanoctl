import { expect, test } from "@playwright/test";
import { execFileSync } from "node:child_process";
import { closePanel, openPanel } from "./panel";

function hostCursor(): { x: number; y: number } {
  const output = execFileSync(
    "python",
    [
      "-c",
      "import ctypes,ctypes.wintypes as w; p=w.POINT(); ctypes.windll.user32.GetCursorPos(ctypes.byref(p)); print(f'{p.x},{p.y}')",
    ],
    { encoding: "utf8" },
  ).trim();
  const [x, y] = output.split(",").map(Number);
  return { x, y };
}

function setHostCursor(x: number, y: number): void {
  execFileSync("python", ["-c", `import ctypes; ctypes.windll.user32.SetCursorPos(${x},${y})`]);
}

test("relative pointer-lock motion moves the Windows host cursor relatively", async ({ page }) => {
  test.skip(process.platform !== "win32", "Windows host cursor validation");
  test.skip(process.env.NANOCTL_BROWSER_MEDIA !== "1", "Requires live media input");
  await page.addInitScript(() => {
    const state = { pointer: null as RTCDataChannel | null };
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
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (window as unknown as { state: { pointer: RTCDataChannel | null } }).state.pointer
            ?.readyState,
      ),
    )
    .toBe("open");

  const sendRelative = async (dx: number, dy: number) => {
    await page.evaluate(
      ({ dx, dy }) => {
        const bytes = new Uint8Array(5);
        const view = new DataView(bytes.buffer);
        view.setUint8(0, 14);
        view.setInt16(1, dx, true);
        view.setInt16(3, dy, true);
        (window as unknown as { state: { pointer: RTCDataChannel } }).state.pointer.send(bytes);
      },
      { dx, dy },
    );
  };

  setHostCursor(500, 500);
  expect(hostCursor()).toEqual({ x: 500, y: 500 });
  await sendRelative(123, 45);
  await expect
    .poll(() => {
      const cursor = hostCursor();
      return cursor.x > 550 && cursor.y > 520;
    })
    .toBe(true);
  await page.waitForTimeout(250);
  await sendRelative(-123, -45);
  // Mouse acceleration makes a single relative displacement display-dependent,
  // but reversing the same motion must return to the starting point. That is
  // the property absolute-position injection cannot accidentally satisfy here.
  await expect.poll(hostCursor).toEqual({ x: 500, y: 500 });
  await closePanel(page);
});

test("absolute pointer motion moves the Windows host cursor", async ({ page }) => {
  test.skip(process.platform !== "win32", "Windows host input validation");
  test.skip(process.env.NANOCTL_BROWSER_MEDIA !== "1", "Requires live media input");
  await page.goto("/");
  await openPanel(page);
  await expect(page.getByTestId("connection-state")).toHaveText("Live");
  await closePanel(page);
  const geometry = await page.evaluate(() => {
    const video = document.querySelector("video")!;
    const rect = video.getBoundingClientRect();
    const scale = Math.min(rect.width / video.videoWidth, rect.height / video.videoHeight);
    const w = video.videoWidth * scale;
    const h = video.videoHeight * scale;
    return { left: rect.left + (rect.width - w) / 2, top: rect.top + (rect.height - h) / 2, w, h };
  });
  const target = { x: 0.67, y: 0.42 };
  await page.mouse.move(
    geometry.left + target.x * geometry.w,
    geometry.top + target.y * geometry.h,
  );
  const size = execFileSync(
    "python",
    [
      "-c",
      "import ctypes; print(f'{ctypes.windll.user32.GetSystemMetrics(0)},{ctypes.windll.user32.GetSystemMetrics(1)}')",
    ],
    { encoding: "utf8" },
  )
    .trim()
    .split(",")
    .map(Number);
  await expect
    .poll(() => {
      const cursor = hostCursor();
      return (
        Math.abs(cursor.x - target.x * (size[0]! - 1)) <= 8 &&
        Math.abs(cursor.y - target.y * (size[1]! - 1)) <= 8
      );
    })
    .toBe(true);
});

test("mouse-lock toggle makes browser pointer motion relative", async ({ page }) => {
  test.skip(process.platform !== "win32", "Windows host input validation");
  test.skip(process.env.NANOCTL_BROWSER_MEDIA !== "1", "Requires live media input");
  await page.addInitScript(() => {
    const state = { sent: [] as number[][] };
    Object.assign(window, { state });
    const send = RTCDataChannel.prototype.send;
    RTCDataChannel.prototype.send = function (data) {
      if (
        this.label === "nanoctl.pointer.v1" &&
        typeof data !== "string" &&
        !(data instanceof Blob)
      ) {
        const bytes =
          data instanceof ArrayBuffer
            ? new Uint8Array(data)
            : new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
        state.sent.push([...bytes]);
      }
      return Reflect.apply(send, this, [data]);
    };
  });
  await page.goto("/");
  await openPanel(page);
  await expect(page.getByTestId("connection-state")).toHaveText("Live");
  const toggle = page.getByRole("button", { name: /Mouse (absolute|locked)/ });
  await expect(toggle).toBeVisible();
  await toggle.click();
  await expect
    .poll(() => page.evaluate(() => document.pointerLockElement instanceof HTMLVideoElement))
    .toBe(true);
  await expect(toggle).toHaveText("Mouse locked");
  await expect(page.getByText("Mouse locked · Ctrl+Alt+M or Esc to release")).toBeVisible();
  await page.evaluate(() => {
    (window as unknown as { state: { sent: number[][] } }).state.sent.length = 0;
  });
  await page.mouse.move(240, 240);
  await page.mouse.move(285, 270);
  await page.mouse.move(250, 310);
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (window as unknown as { state: { sent: number[][] } }).state.sent.filter(
            (bytes) => bytes[0] === 14,
          ).length,
      ),
    )
    .toBeGreaterThan(0);
  const absoluteMoves = await page.evaluate(
    () =>
      (window as unknown as { state: { sent: number[][] } }).state.sent.filter(
        (bytes) => bytes[0] === 1,
      ).length,
  );
  expect(absoluteMoves).toBe(0);
  await page.evaluate(() => document.exitPointerLock());
  await expect(toggle).toHaveText("Mouse absolute");
  await expect(page.getByText("Mouse locked · Ctrl+Alt+M or Esc to release")).toBeHidden();
  await closePanel(page);
});
