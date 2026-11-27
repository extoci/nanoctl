import { expect, test } from "@playwright/test";
import { execFileSync } from "node:child_process";
import { closePanel, openPanel } from "./panel";

function agentPriorityClasses(): string[] {
  const output = execFileSync(
    "powershell",
    [
      "-NoProfile",
      "-Command",
      "(Get-Process -Name nanoctl -ErrorAction SilentlyContinue | Select-Object -ExpandProperty PriorityClass) -join ','",
    ],
    { encoding: "utf8" },
  ).trim();
  return output ? output.split(",") : [];
}

test("active streaming raises the Windows agent process priority", async ({ page }) => {
  test.skip(process.platform !== "win32", "Windows scheduling validation");
  test.skip(process.env.NANOCTL_BROWSER_MEDIA !== "1", "Requires live media input");
  await page.goto("/");
  await openPanel(page);
  await expect(page.getByTestId("connection-state")).toHaveText("Live");
  await expect.poll(agentPriorityClasses).toContain("High");
  await closePanel(page);
});
