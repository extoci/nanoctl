import { expect, test } from "@playwright/test";
import { readFile } from "node:fs/promises";
import { openPanel } from "./panel";

test("forwards gamepad reports only while enabled and detaches on disconnect", async ({ page }) => {
  const reports = process.env.NANOCTL_DEV_GAMEPAD_REPORTS;
  test.skip(!reports, "Requires the media agent with a mock virtual gamepad");
  const events = () => readFile(reports!, "utf8").catch(() => "");
  await page.addInitScript(() => {
    const fake = {
      controllerAttached: true,
      getGamepads() {
        return this.controllerAttached
          ? [
              {
                axes: [0.5, -1, 0, 0],
                buttons: Array.from({ length: 17 }, (_, i) => ({
                  pressed: i === 0,
                  value: i === 0 ? 1 : i === 6 ? 0.5 : 0,
                })),
                connected: true,
                id: "Xbox Wireless Controller",
                index: 0,
                mapping: "standard",
              },
              {
                axes: [0.5, -1, 0, 0],
                buttons: Array.from({ length: 18 }, (_, i) => ({
                  pressed: [0, 6, 8, 9, 12, 15, 16, 17].includes(i),
                  value: i === 6 ? 0.5 : 0,
                })),
                connected: true,
                id: "DualSense Wireless Controller Extended Gamepad",
                index: 1,
                mapping: "standard",
              },
            ]
          : [];
      },
    };
    Object.assign(window, { fake });
    Object.defineProperty(navigator, "getGamepads", { value: () => fake.getGamepads() });
  });
  await page.goto("/");
  await openPanel(page);
  await expect(page.getByTestId("connection-state")).toHaveText("Live");
  const gamepad = page.getByRole("button", { name: "Gamepad off" });
  await expect(gamepad).toBeEnabled();
  expect(await events()).toBe("");
  await gamepad.click();
  await expect.poll(events).toContain("0 attached xbox360");
  await expect.poll(events).toContain("0 4096 128 0 16384 32767 0 0");
  await expect.poll(events).toContain("1 attached dualShock4");
  await expect.poll(events).toContain("1 dualShock4 13345 3 128 0 192 0 128 128");
  await page.evaluate(() => {
    (window as unknown as { fake: { controllerAttached: boolean } }).fake.controllerAttached =
      false;
  });
  await expect.poll(events).toContain("0 detached");
  await expect.poll(events).toContain("1 detached");
  await page.evaluate(() => {
    (window as unknown as { fake: { controllerAttached: boolean } }).fake.controllerAttached = true;
  });
  await expect.poll(async () => (await events()).match(/0 attached/g)?.length).toBe(2);
  await page.getByRole("button", { name: "Input on" }).click();
  await expect.poll(async () => (await events()).match(/0 detached/g)?.length).toBe(2);
  await expect(gamepad).toBeDisabled();
});
