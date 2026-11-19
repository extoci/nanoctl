import { test, expect, chromium } from "@playwright/test";
import { readFile } from "node:fs/promises";
import { copyFile, mkdir } from "node:fs/promises";
import { homedir } from "node:os";
import { join } from "node:path";
import { closePanel, openPanel } from "./panel";

test("host picker lists tailnet machines and supports keyboard", async ({ page }, testInfo) => {
  await page.route("**/v1/hosts", (route) =>
    route.fulfill({
      json: {
        self: {
          hostName: "Current desktop",
          dnsName: "current.ts.net",
          url: "https://current.ts.net:7337/",
        },
        hosts: [
          {
            hostName: "Build server",
            dnsName: "build.ts.net",
            os: "linux",
            online: true,
            url: "https://build.ts.net:7337/",
          },
          {
            hostName: "Studio Mac",
            dnsName: "studio.ts.net",
            os: "macOS",
            online: true,
            url: "https://studio.ts.net:7337/",
          },
        ],
      },
    }),
  );
  await page.goto("/");
  await openPanel(page);
  const picker = page.getByRole("button", { name: "Choose host" });
  await expect(picker).toContainText("(3)");
  const directory = join(homedir(), "picker-shots");
  await mkdir(directory, { recursive: true });
  const closed = testInfo.outputPath("picker-closed.png");
  await page.screenshot({ path: closed });
  await copyFile(closed, join(directory, "picker-closed.png"));
  await picker.click();
  const hosts = page.getByRole("group", { name: "Tailnet hosts" });
  await expect(hosts).toContainText("Current desktop");
  await expect(hosts).toContainText("Build server");
  await expect(hosts).toContainText("Studio Mac");
  await expect(hosts).toContainText("macOS");
  const opened = testInfo.outputPath("picker-open.png");
  await page.screenshot({ path: opened });
  await copyFile(opened, join(directory, "picker-open.png"));
  await picker.press("ArrowDown");
  await expect(hosts.getByRole("button").first()).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(picker).toBeFocused();
  await expect(hosts).toBeHidden();
});

test("an unpaced 200-character key burst reaches native input in order", async ({ page }) => {
  test.skip(
    process.env.NANOCTL_BROWSER_MEDIA !== "1" ||
      process.platform !== "linux" ||
      !process.env.DISPLAY,
    "Requires Linux native input and an X display",
  );
  await page.addInitScript(() => {
    const create = RTCPeerConnection.prototype.createDataChannel;
    RTCPeerConnection.prototype.createDataChannel = function (...args) {
      const channel = Reflect.apply(create, this, args) as RTCDataChannel;
      if (args[0] === "nanoctl.control.v1")
        (window as unknown as { control: RTCDataChannel }).control = channel;
      return channel;
    };
  });
  const targetBrowser = await chromium.launch({ headless: false });
  try {
    const target = await targetBrowser.newPage();
    await target.setContent('<input aria-label="Native input target" autofocus>');
    await page.goto("/");
    await expect(page.getByTestId("connection-state")).toHaveText("Live");
    await target.bringToFront();
    await target.getByRole("textbox").focus();
    const text = "abcdefghijklmnopqrstuvwxyz".repeat(8).slice(0, 200);
    await page.evaluate((text) => {
      const channel = (window as unknown as { control: RTCDataChannel }).control;
      for (const key of text) {
        for (const action of ["down", "up"])
          channel.send(
            JSON.stringify({
              type: "key",
              action,
              code: `Key${key.toUpperCase()}`,
              key,
              repeat: false,
            }),
          );
      }
    }, text);
    await expect(target.getByRole("textbox")).toHaveValue(text, {
      timeout: 5000,
    });
    let expected = text;
    const commitText = async (label: string, commit: string, timeout: number) => {
      const started = performance.now();
      await page.evaluate((text) => {
        (window as unknown as { control: RTCDataChannel }).control.send(
          JSON.stringify({ type: "text", text }),
        );
      }, commit);
      expected += commit;
      await target
        .waitForFunction(
          (expected) => document.querySelector("input")?.value === expected,
          expected,
          { timeout },
        )
        .catch(async (error: unknown) => {
          console.log(
            `${label} received: ${JSON.stringify(await target.getByRole("textbox").inputValue())}`,
          );
          throw error;
        });
      const elapsed = performance.now() - started;
      console.log(`${label}: ${elapsed.toFixed(1)} ms`);
      expect(elapsed).toBeLessThan(timeout);
    };
    await commitText(
      "200-character ASCII/Latin commit",
      "AbC xyz 123!? ".repeat(20).slice(0, 200),
      1000,
    );
    await commitText(
      "200-character mixed Unicode commit",
      Array.from("Hello 日本語 café 🙂! ".repeat(20)).slice(0, 200).join(""),
      1000,
    );
    const distinct = Array.from({ length: 200 }, (_, index) =>
      String.fromCodePoint(0x4e00 + index),
    ).join("");
    for (let attempt = 1; attempt <= 3; attempt++)
      await commitText(`200-distinct-CJK commit ${attempt}`, distinct, 3000);
  } finally {
    await targetBrowser.close();
  }
});

test("disabled input stays off across repeated video clicks", async ({ page }) => {
  test.skip(process.env.NANOCTL_BROWSER_MEDIA !== "1", "Requires live video");
  await page.addInitScript(() => {
    const send = RTCDataChannel.prototype.send;
    (window as unknown as { injected: string[] }).injected = [];
    RTCDataChannel.prototype.send = function (data: unknown) {
      (window as unknown as { injected: string[] }).injected.push(String(data));
      return Reflect.apply(send, this, [data]);
    };
  });
  await page.goto("/");
  await expect(page.getByTestId("connection-state")).toHaveText("Live");
  await openPanel(page);
  await page.getByRole("button", { name: "Input on", exact: true }).click();
  await closePanel(page);
  await page.evaluate(() => {
    (window as unknown as { injected: string[] }).injected = [];
  });
  await page.locator("video").click();
  await page.locator("video").click();
  if (process.platform !== "win32") {
    const pid = Number(await readFile(process.env.NANOCTL_BROWSER_AGENT_PID_FILE!, "utf8"));
    process.kill(pid, "SIGUSR2");
    await expect(page.getByTestId("connection-state")).toHaveText("Reconnecting");
    await expect(page.getByTestId("connection-state")).toHaveText("Live", {
      timeout: 30_000,
    });
    await page.locator("video").click();
    await page.locator("video").click();
    await page.keyboard.press("a");
  }
  await openPanel(page);
  await expect(page.getByRole("button", { name: "Input off", exact: true })).toHaveAttribute(
    "aria-pressed",
    "false",
  );
  expect(
    await page.evaluate(() =>
      (window as unknown as { injected: string[] }).injected.filter((data) =>
        /"type":"(?:pointer|key)"/.test(data),
      ),
    ),
  ).toEqual([]);
});

test("graceful host shutdown and restart recovers automatically", async ({ page }) => {
  test.skip(
    process.env.NANOCTL_BROWSER_MEDIA !== "1" || process.platform === "win32",
    "Requires live video and POSIX signals",
  );
  await page.goto("/");
  await expect(page.getByTestId("connection-state")).toHaveText("Live");
  const pid = Number(await readFile(process.env.NANOCTL_BROWSER_AGENT_PID_FILE!, "utf8"));
  process.kill(pid, "SIGUSR2");
  await expect(page.getByTestId("connection-state")).toHaveText("Reconnecting");
  await expect(page.getByTestId("connection-state")).toHaveText("Live", {
    timeout: 30_000,
  });
});

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
  await openPanel(page);
  const head = page.locator(".panel-head");
  await expect(head.getByText(info.hostName, { exact: true })).toBeVisible();
  await expect(head.getByText("dev@example.com", { exact: true })).toBeVisible();
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
  await openPanel(page);
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

test("session panel sends shortcuts, switches scale and disconnects cleanly", async ({ page }) => {
  test.skip(process.env.NANOCTL_BROWSER_MEDIA !== "1", "Requires live video");
  await page.addInitScript(() => {
    const send = RTCDataChannel.prototype.send;
    (window as unknown as { injected: string[] }).injected = [];
    RTCDataChannel.prototype.send = function (data: unknown) {
      (window as unknown as { injected: string[] }).injected.push(String(data));
      return Reflect.apply(send, this, [data]);
    };
  });
  const directory = join(homedir(), "crd-shots");
  await mkdir(directory, { recursive: true });
  await page.goto("/");
  await expect(page.getByTestId("connection-state")).toHaveText("Live", { timeout: 30_000 });
  await page.screenshot({ path: join(directory, "1-live-fullbleed.png") });
  await openPanel(page);
  await page.getByRole("button", { name: "Stats", exact: true }).click();
  await page.screenshot({ path: join(directory, "2-session-panel.png") });
  await page.evaluate(() => {
    (window as unknown as { injected: string[] }).injected = [];
  });
  await page.getByRole("group", { name: "Send keys" }).getByRole("button").first().click();
  await expect
    .poll(async () =>
      (await page.evaluate(() => (window as unknown as { injected: string[] }).injected))
        .map((data) => JSON.parse(data))
        .filter((m) => m.type === "key")
        .map((m) => `${m.action}:${m.code}`),
    )
    .toEqual(["down:AltLeft", "down:Tab", "up:Tab", "up:AltLeft"]);
  await page.getByRole("button", { name: "Shrink to fit", exact: true }).click();
  await expect(page.getByRole("button", { name: "Original size", exact: true })).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  const size = await page
    .locator("video")
    .evaluate((v) => [(v as HTMLVideoElement).clientWidth, (v as HTMLVideoElement).videoWidth]);
  expect(size[0]).toBe(size[1]);
  await page.getByRole("button", { name: "Original size", exact: true }).click();
  await page.getByRole("button", { name: "Disconnect", exact: true }).click();
  await expect(page.getByRole("heading", { name: "You ended the session." })).toBeVisible();
  await expect(page.getByTestId("connection-state")).toHaveAttribute("data-phase", "disconnected");
  await page.screenshot({ path: join(directory, "3-disconnected.png") });
  await page.getByRole("button", { name: "Reconnect", exact: true }).click();
  await expect(page.getByTestId("connection-state")).toHaveText("Live", { timeout: 30_000 });
});

test("a closed session panel is out of the tab order", async ({ page }) => {
  await page.goto("/");
  await openPanel(page);
  await closePanel(page);
  for (let i = 0; i < 6; i++) await page.keyboard.press("Tab");
  expect(
    await page.evaluate(() => Boolean(document.activeElement?.closest(".session-panel"))),
  ).toBe(false);
  expect(await page.evaluate(() => document.querySelector(".viewer")!.scrollLeft)).toBe(0);
});
