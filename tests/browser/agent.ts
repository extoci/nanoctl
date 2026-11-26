import { spawn } from "node:child_process";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { homedir } from "node:os";
import { join } from "node:path";

const cache = join(homedir(), ".cache");
await mkdir(cache, { recursive: true });
const directory = await mkdtemp(join(cache, "nanoctl-browser-"));
const config = join(directory, "config.toml");
const port = Number(process.env.NANOCTL_BROWSER_PORT);
const latency = process.env.NANOCTL_BROWSER_LATENCY === "1";
if (!Number.isInteger(port) || port < 1 || port > 65535)
  throw new Error("Browser-test port required");
await writeFile(
  config,
  `[tailnet]
port = ${port}
tls = "off"
allow_users = ["dev@example.com"]
serve_loopback = false
[quality]
encoder = "auto"
max_width = ${latency ? 1920 : 1280}
max_height = ${latency ? 1080 : 720}
max_fps = ${latency ? 60 : 30}
`,
);
const env = {
  ...process.env,
  NANOCTL_DEV_LOOPBACK_USER: "dev@example.com",
  XDG_DATA_HOME: directory,
  XDG_CONFIG_HOME: directory,
  ...(process.platform === "linux" && !process.env.CC ? { CC: "gcc" } : {}),
};
const prebuilt = process.env.NANOCTL_BROWSER_AGENT_BIN;
const start = () =>
  spawn(
    prebuilt ?? "cargo",
    prebuilt
      ? ["--config", config, "run"]
      : [
          "run",
          "--features",
          latency || process.env.NANOCTL_BROWSER_MEDIA === "1" ? "media" : "rtc",
          ...(latency ? ["--release", "--locked"] : []),
          "--manifest-path",
          "crates/nanoctl-agent/Cargo.toml",
          "--",
          "--config",
          config,
          "run",
        ],
    { env, stdio: "inherit" },
  );
let child = start();
if (process.env.NANOCTL_BROWSER_AGENT_PID_FILE)
  await writeFile(process.env.NANOCTL_BROWSER_AGENT_PID_FILE, String(process.pid));
let restarting = false;
process.on("SIGUSR2", () => {
  restarting = true;
  child.kill("SIGINT");
});
let stopping = false;
const stop = () => {
  stopping = true;
  child.kill("SIGINT");
};
process.on("SIGTERM", stop);
process.on("SIGINT", stop);
try {
  let code: number;
  do {
    restarting = false;
    code = await new Promise<number>((resolve, reject) => {
      child.once("error", reject);
      child.once("exit", (code) => resolve(code ?? (stopping ? 0 : 1)));
    });
    if (restarting && !stopping) {
      await new Promise((resolve) => setTimeout(resolve, 1500));
      child = start();
    } else break;
  } while (!stopping);
  process.exitCode = code;
} finally {
  await rm(directory, { recursive: true, force: true });
}
