import { test, expect } from "@playwright/test";

test("embedded viewer authenticates and negotiates under its CSP", async ({
  page,
  request,
}, testInfo) => {
  const errors: string[] = [];
  const remoteRequests: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  page.on("console", (message) => {
    if (message.type() === "error") errors.push(message.text());
  });
  page.on("request", (request) => {
    if (new URL(request.url()).origin !== new URL(testInfo.project.use.baseURL!).origin)
      remoteRequests.push(request.url());
  });
  const frames: string[] = [];
  page.on("websocket", (socket) => {
    socket.on("framereceived", (event) => frames.push(String(event.payload)));
  });
  const infoResponse = await request.get("/v1/info");
  expect(infoResponse.ok()).toBe(true);
  const info = await infoResponse.json();
  expect(info.viewer.identity.login).toBe("dev@example.com");
  const response = await page.goto("/");
  expect(response?.headers()["content-security-policy"]).toContain("script-src 'self'");
  expect(response?.headers()["content-security-policy"]).not.toContain("unsafe-eval");
  await expect(page).toHaveTitle(`${info.hostName} · nanoctl`);
  await expect(page.getByText(info.hostName, { exact: true })).toBeVisible();
  await expect(page.getByText("dev@example.com", { exact: true })).toBeVisible();
  await expect
    .poll(() =>
      frames.some((frame) => {
        const message = JSON.parse(frame);
        return message.type === "signal" && message.envelope.payload.type === "answer";
      }),
    )
    .toBe(true);
  await expect(page.getByTestId("connection-state")).toHaveAttribute(
    "data-phase",
    /negotiating|connected/,
  );
  if (process.env.NANOCTL_BROWSER_MEDIA === "1") {
    await expect
      .poll(
        () => page.locator("video").evaluate((video) => (video as HTMLVideoElement).videoWidth),
        { timeout: 30_000 },
      )
      .toBeGreaterThan(0);
    await expect(page.getByTestId("connection-state")).toHaveText("Live");
  } else if (info.displays.length === 0) {
    await expect(page.getByText(/The host has no displays/)).toBeVisible();
  }
  await page.getByRole("button", { name: "Stats", exact: true }).click();
  await expect(page.getByLabel("Connection statistics")).toContainText("RTT");
  if (process.env.NANOCTL_BROWSER_MEDIA === "1") {
    await expect
      .poll(
        async () => {
          const stats = await page.getByLabel("Connection statistics").innerText();
          return /Bitrate\s+(?!0\.00)[\d.]+ kbps/.test(stats);
        },
        { timeout: 15_000 },
      )
      .toBe(true);
  }
  await page.screenshot({ path: testInfo.outputPath("viewer.png") });
  expect(remoteRequests).toEqual([]);
  expect(errors).toEqual([]);
});

test("a newer viewer replaces the old session without an automatic takeover", async ({
  page,
  context,
}) => {
  await page.goto("/");
  await expect(page.getByTestId("connection-state")).toHaveAttribute(
    "data-phase",
    /negotiating|connected/,
  );
  const next = await context.newPage();
  await next.goto("/");
  await expect(page.getByRole("heading", { name: "Another viewer took over" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Reconnect", exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Reconnect", exact: true }).click();
  await expect(next.getByRole("heading", { name: "Another viewer took over" })).toBeVisible();
  await next.close();
});

test("WebSocket upgrades reject unauthorized Origins", async ({ request }) => {
  const response = await request.get("/v1/connect", {
    headers: {
      Connection: "Upgrade",
      Upgrade: "websocket",
      "Sec-WebSocket-Version": "13",
      "Sec-WebSocket-Key": "dGhlIHNhbXBsZSBub25jZQ==",
      Origin: "https://unauthorized.invalid",
    },
  });
  expect(response.status()).toBe(403);
  expect(await response.text()).toContain("invalid WebSocket Origin");
});

test("403 explanations from the agent are shown verbatim", async ({ page }) => {
  await page.route("**/v1/info", (route) =>
    route.fulfill({
      status: 403,
      body: "identity=dev@example.com: access denied by tailnet policy",
    }),
  );
  await page.goto("/");
  await expect(
    page.getByRole("heading", {
      name: "identity=dev@example.com: access denied by tailnet policy",
    }),
  ).toBeVisible();
});

test("unsupported browsers get an actionable error", async ({ page }) => {
  await page.addInitScript(() => {
    Object.defineProperty(window, "RTCPeerConnection", { value: undefined });
  });
  await page.goto("/");
  await expect(page.getByRole("heading", { name: /Browser unsupported/ })).toBeVisible();
});
