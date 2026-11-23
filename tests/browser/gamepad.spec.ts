import { expect, test } from "@playwright/test";
import { readFile } from "node:fs/promises";

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
                index: 0,
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
  await expect(page.getByTestId("connection-state")).toHaveText("Live");
  const gamepad = page.getByRole("button", { name: "Gamepad off" });
  await expect(gamepad).toBeEnabled();
  expect(await events()).toBe("");
  await gamepad.click();
  await expect.poll(events).toContain("0 4096 128 0 16384 32767 0 0");
  await page.evaluate(() => {
    (window as unknown as { fake: { controllerAttached: boolean } }).fake.controllerAttached =
      false;
  });
  await expect.poll(events).toContain("0 detached");
  await page.evaluate(() => {
    (window as unknown as { fake: { controllerAttached: boolean } }).fake.controllerAttached = true;
  });
  await expect.poll(async () => (await events()).match(/0 attached/g)?.length).toBe(2);
  await page.getByRole("button", { name: "Input on" }).click();
  await expect.poll(async () => (await events()).match(/0 detached/g)?.length).toBe(2);
  await expect(gamepad).toBeDisabled();
});
