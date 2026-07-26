import { expect, test } from "@playwright/test";

test("every document receives hardened headers and matching script nonces", async ({ page }) => {
  const response = await page.goto("/");
  expect(response).not.toBeNull();
  const headers = response!.headers();
  expect(headers["x-content-type-options"]).toBe("nosniff");
  expect(headers["x-frame-options"]).toBe("DENY");
  const policy = headers["content-security-policy"];
  expect(policy).toContain("frame-ancestors 'none'");
  expect(policy).toContain("https://shoo.dev");
  const nonce = policy.match(/'nonce-([^']+)'/)?.[1];
  expect(nonce).toBeTruthy();

  const scriptNonces = await page
    .locator("script")
    .evaluateAll((scripts) => scripts.map((script) => script.nonce).filter(Boolean));
  expect(scriptNonces.length).toBeGreaterThan(0);
  expect(new Set(scriptNonces)).toEqual(new Set([nonce]));
});
