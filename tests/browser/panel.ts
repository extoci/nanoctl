import { expect, type Page } from "@playwright/test";

async function setPanel(page: Page, open: boolean) {
  const tab = page.getByRole("button", { name: "Session options" });
  if ((await tab.getAttribute("aria-expanded")) !== String(open)) await tab.click();
  await expect(tab).toHaveAttribute("aria-expanded", String(open));
  await page.waitForTimeout(250);
}
export const openPanel = (page: Page) => setPanel(page, true);
export const closePanel = (page: Page) => setPanel(page, false);
