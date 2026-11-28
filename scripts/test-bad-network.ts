import { spawn } from "node:child_process";

const child = spawn(
  process.execPath,
  [
    "x",
    "playwright",
    "test",
    "-c",
    "tests/browser/playwright.config.ts",
    "bad-network.spec.ts",
    ...process.argv.slice(2),
  ],
  {
    stdio: "inherit",
    env: {
      ...process.env,
      NANOCTL_BROWSER_MEDIA: "1",
      NANOCTL_BROWSER_BAD_NETWORK: "1",
    },
  },
);
child.once("exit", (code) => {
  process.exitCode = code ?? 1;
});
