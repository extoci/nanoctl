import { defineConfig, devices } from "@playwright/test";
import { createServer } from "node:net";
import { join } from "node:path";
import { homedir } from "node:os";

process.env.NANOCTL_BROWSER_AGENT_PID_FILE ??= join(
  homedir(),
  ".cache",
  `nanoctl-browser-agent-${process.pid}.pid`,
);

if (!process.env.NANOCTL_BROWSER_PORT) {
  const server = createServer();
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("No free browser-test port");
  process.env.NANOCTL_BROWSER_PORT = String(address.port);
  await new Promise<void>((resolve, reject) =>
    server.close((error) => (error ? reject(error) : resolve())),
  );
}
const baseURL = `http://127.0.0.1:${process.env.NANOCTL_BROWSER_PORT}`;

export default defineConfig({
  tsconfig: "./tsconfig.json",
  testDir: ".",
  fullyParallel: false,
  workers: 1,
  timeout: 45_000,
  forbidOnly: Boolean(process.env.CI),
  retries: process.env.CI ? 2 : 0,
  reporter: process.env.CI ? "github" : "list",
  use: {
    baseURL,
    trace: "retain-on-failure",
  },
  webServer: {
    cwd: "../..",
    command: "bun tests/browser/agent.ts",
    url: baseURL + "/v1/info",
    reuseExistingServer: false,
    timeout: 300_000,
    gracefulShutdown: { signal: "SIGTERM", timeout: 15_000 },
  },
  projects: [
    {
      name: "chromium",
      use: { ...devices["Desktop Chrome"] },
    },
  ],
});
