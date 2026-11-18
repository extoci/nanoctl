import { expect, test } from "@playwright/test";

for (const failure of ["clipboard rejects", "execCommand throws", "execCommand returns false"]) {
  test(`pairing copy reports failure and cleans up when ${failure}`, async ({ page }) => {
    await page.addInitScript((mode) => {
      Object.defineProperty(navigator, "clipboard", {
        configurable: true,
        value:
          mode === "clipboard rejects"
            ? {
                writeText: async () => {
                  throw new Error("Clipboard denied");
                },
              }
            : undefined,
      });
      document.execCommand = () => {
        if (mode === "execCommand throws") throw new Error("Copy unavailable");
        return false;
      };
    }, failure);
    await page.goto("/e2e/dashboard?state=empty");
    await page.getByRole("button", { name: "Add device" }).click();
    await page.getByRole("button", { name: "Copy code" }).click();
    await expect
      .soft(page.locator(".inline-error[role=alert]"))
      .toHaveText("Could not copy the setup code. Select and copy it manually.");
    await expect(page.locator("textarea")).toHaveCount(0);
    await expect(page.getByRole("button", { name: "Copy code" })).toBeVisible();

    await page.evaluate(() => {
      Object.defineProperty(navigator, "clipboard", {
        configurable: true,
        value: { writeText: async () => {} },
      });
    });
    await page.getByRole("button", { name: "Copy code" }).click();
    await expect(page.getByRole("button", { name: "Copied", exact: true })).toBeVisible();
    await expect(page.locator(".inline-error[role=alert]")).toHaveCount(0);
  });
}

test("pairing copy fallback succeeds without leaving a textarea", async ({ page }) => {
  await page.addInitScript(() => {
    Object.defineProperty(navigator, "clipboard", { value: undefined });
    document.execCommand = () => true;
  });
  await page.goto("/e2e/dashboard?state=empty");
  await page.getByRole("button", { name: "Add device" }).click();
  await page.getByRole("button", { name: "Copy code" }).click();
  await expect(page.getByRole("button", { name: "Copied", exact: true })).toBeVisible();
  await expect(page.locator("textarea")).toHaveCount(0);
  await expect(page.locator(".inline-error[role=alert]")).toHaveCount(0);
});

test("authenticated dashboard renders loading and empty states", async ({ page }) => {
  await page.goto("/e2e/dashboard?state=loading");
  await expect(page.getByText("Loading devices…")).toBeVisible();
  await expect(page.getByText("Loading activity…")).toBeVisible();

  await page.goto("/e2e/dashboard?state=empty");
  await expect(page.getByRole("heading", { name: "No devices yet" })).toBeVisible();
  await expect(page.getByText("No access events yet.")).toBeVisible();
});

test("authenticated dashboard distinguishes readiness and exposes security activity", async ({
  page,
}) => {
  await page.goto("/e2e/dashboard?state=mixed");

  const ready = page.locator(".device-card").filter({ hasText: "Studio workstation" });
  const unready = page.locator(".device-card").filter({ hasText: "Linux laptop" });
  const offline = page.locator(".device-card").filter({ hasText: "Travel Mac" });
  await expect(ready.getByRole("button", { name: "Connect" })).toBeEnabled();
  await expect(unready.getByText("needs input permission")).toBeVisible();
  await expect(unready.getByRole("button", { name: "Connect" })).toBeDisabled();
  await expect(offline.getByText("offline", { exact: true })).toBeVisible();
  await expect(offline.getByRole("button", { name: "Connect" })).toBeDisabled();
  await expect(page.getByText("Session Connected")).toBeVisible();
  await expect(page.getByText("Studio workstation").last()).toBeVisible();
});

test("authenticated dashboard supports pairing, rename, removal, and sign-out interactions", async ({
  page,
}) => {
  await page.goto("/e2e/dashboard?state=mixed");

  await page.getByRole("button", { name: "Add device" }).click();
  await expect(page.locator(".pairing-code")).toHaveText("ABCDE-FGHJK-MNPQR-STVWX");
  await page.getByRole("button", { name: "Done" }).click();
  await expect(page.locator(".pairing-code")).toBeHidden();

  const ready = page.locator(".device-card").filter({ hasText: "Studio workstation" });
  page.once("dialog", async (dialog) => dialog.accept("Editing workstation"));
  await ready.getByRole("button", { name: "Rename" }).click();
  await expect(page.getByRole("heading", { name: "Editing workstation" })).toBeVisible();

  const renamed = page.locator(".device-card").filter({ hasText: "Editing workstation" });
  page.once("dialog", async (dialog) => dialog.accept());
  await renamed.getByRole("button", { name: "Remove" }).click();
  await expect(renamed.getByText("disabled", { exact: true })).toBeVisible();
  await expect(renamed.getByRole("button", { name: "Connect" })).toBeDisabled();
  await expect(renamed.getByRole("button", { name: "Remove" })).toBeDisabled();

  await page.getByRole("button", { name: "Sign out" }).click();
  await expect(page.getByRole("heading", { name: "Fixture signed out" })).toBeVisible();
});

test("authenticated dashboard contains operation failures", async ({ page }) => {
  await page.goto("/e2e/dashboard?state=errors");
  const error = page.locator(".inline-error");
  await page.getByRole("button", { name: "Add device" }).click();
  await expect(error).toHaveText("Could not create a setup code. Try again.");

  const ready = page.locator(".device-card").filter({ hasText: "Studio workstation" });
  await ready.getByRole("button", { name: "Connect" }).click();
  await expect(error).toHaveText("Could not start the remote session.");

  page.once("dialog", async (dialog) => dialog.accept("Broken rename"));
  await ready.getByRole("button", { name: "Rename" }).click();
  await expect(error).toHaveText("Could not rename the device.");
  await expect(page.getByRole("heading", { name: "Studio workstation" })).toBeVisible();
});
