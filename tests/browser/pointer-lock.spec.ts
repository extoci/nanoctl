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
