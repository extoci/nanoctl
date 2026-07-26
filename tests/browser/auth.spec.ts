import { expect, test } from "@playwright/test";

test("unauthenticated pages remain inside the login gate", async ({ page }) => {
  await page.goto("/");
  await expect(page.getByRole("heading", { level: 1 })).toHaveText(
    "Your computers, one quiet click away.",
  );
  await page.goto("/dashboard");
  await expect(page.getByRole("button", { name: "Continue with Shoo" })).toBeVisible();
});

test("Shoo sign-in binds the callback to this exact origin", async ({ page }) => {
  let authorizeUrl: URL | undefined;
  await page.route("https://shoo.dev/**", async (route) => {
    authorizeUrl = new URL(route.request().url());
    await route.abort();
  });
  await page.goto("/");
  await page.getByRole("button", { name: "Continue with Shoo" }).click();
  await expect.poll(() => authorizeUrl?.pathname).toBe("/authorize");
  expect(authorizeUrl?.searchParams.get("redirect_uri")).toBe(
    "http://127.0.0.1:3000/auth/callback",
  );
  expect(authorizeUrl?.searchParams.get("client_id")).toBe("origin:http://127.0.0.1:3000");
});

test("malformed callback input cannot navigate away from nanoctl", async ({ page }) => {
  await page.goto("/auth/callback?code=invalid&state=invalid");
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Completing sign-in…");
  expect(new URL(page.url()).origin).toBe("http://127.0.0.1:3000");
});
