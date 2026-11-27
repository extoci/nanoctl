import { expect, test } from "@playwright/test";
import { openPanel } from "./panel";

test("the copied v4 debug report contains live video measurements and the host encoder", async ({
  context,
  page,
}) => {
  test.skip(
    process.env.NANOCTL_BROWSER_MEDIA !== "1" || process.platform !== "linux",
    "Requires the Linux media runtime",
  );
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  await page.goto("/");
  await expect(page.getByTestId("connection-state")).toHaveText("Live");
  await openPanel(page);
  await page.getByRole("button", { name: "Copy debug info" }).click();
  await expect(page.getByRole("button", { name: "Copied", exact: true })).toBeVisible();
  const report = await page.evaluate(() => navigator.clipboard.readText());
  expect(report).toMatch(/decoded resolution: [1-9]\d*×[1-9]\d*/);
  expect(report).toMatch(/WebCodecs submit to output: \d+\.\d+ ms \([1-9]\d* samples\)/);
  expect(report).toContain("last reported encoder backend: software-openh264");
});
