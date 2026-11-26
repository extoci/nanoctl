import { expect, test } from "@playwright/test";
import { readFile, readdir } from "node:fs/promises";

const cycles = 100;
const warmupCycles = 20;
const rssBudgetKiB = 24 * 1024;

async function agentResources(pid: number) {
  const [status, descriptors] = await Promise.all([
    readFile(`/proc/${pid}/status`, "utf8"),
    readdir(`/proc/${pid}/fd`),
  ]);
  const rss = /^VmRSS:\s+(\d+) kB$/m.exec(status)?.[1];
  const threads = /^Threads:\s+(\d+)$/m.exec(status)?.[1];
  if (!rss || !threads) throw new Error("Could not read nanoctl process counters");
  return { rssKiB: Number(rss), threads: Number(threads), descriptors: descriptors.length };
}

test("the real v4 viewer reconnects 100 times without accumulating host resources", async ({
  page,
}, testInfo) => {
  test.skip(
    process.env.NANOCTL_BROWSER_MEDIA !== "1" || process.platform !== "linux",
    "Requires live v4 media and Linux process counters",
  );
  test.setTimeout(240_000);
  const pidFile = process.env.NANOCTL_BROWSER_AGENT_PID_FILE;
  if (!pidFile) throw new Error("Browser-test agent PID file is missing");
  const pid = Number(await readFile(pidFile + ".child", "utf8"));
  expect((await readFile(`/proc/${pid}/comm`, "utf8")).trim()).toBe("nanoctl");

  const states = page.getByTestId("connection-state");
  const panel = page.getByRole("button", { name: "Session options" });
  const disconnect = page.getByRole("button", { name: "Disconnect", exact: true });
  const reconnect = page.getByRole("button", { name: "Reconnect", exact: true });
  const dimensions = () =>
    page.locator("video").evaluate((video) => (video as HTMLVideoElement).videoWidth);

  async function expectLive(label: string) {
    await expect(states, `${label} must receive a frame`).toHaveText("Live", { timeout: 8_000 });
    await expect.poll(dimensions, { message: `${label} must present video` }).toBeGreaterThan(0);
  }

  await page.goto("/");
  await expectLive("initial attach");
  let baseline: Awaited<ReturnType<typeof agentResources>> | undefined;
  const samples = [];
  for (let cycle = 1; cycle <= cycles; cycle += 1) {
    await panel.click();
    await disconnect.click();
    await expect(reconnect).toBeVisible();
    await expect.poll(dimensions).toBe(0);

    if (cycle % warmupCycles === 0) {
      if (baseline) {
        const limit = {
          descriptors: baseline.descriptors + 4,
          threads: baseline.threads + 2,
        };
        await expect
          .poll(
            async () => {
              const sample = await agentResources(pid);
              return sample.descriptors <= limit.descriptors && sample.threads <= limit.threads;
            },
            { timeout: 8_000, message: `cycle ${cycle} must release its host workers and sockets` },
          )
          .toBe(true);
      }
      const sample = await agentResources(pid);
      baseline ??= sample;
      samples.push({ cycle, ...sample });
    }
    await reconnect.click();
    await expectLive(`reattach ${cycle}`);
  }

  await testInfo.attach("nanoctl-agent-resources", {
    body: JSON.stringify(samples, null, 2),
    contentType: "application/json",
  });
  const rss = samples.map((sample) => sample.rssKiB);
  expect(Math.max(...rss) - rss[0]!, "post-warmup RSS growth in KiB").toBeLessThanOrEqual(
    rssBudgetKiB,
  );
});
