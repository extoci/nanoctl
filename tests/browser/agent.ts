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
const start = () =>
  spawn(
    "cargo",
    [
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
const pidFile = process.env.NANOCTL_BROWSER_AGENT_PID_FILE;
const childPidFile = pidFile && process.platform === "linux" ? pidFile + ".child" : undefined;
async function recordAgentPid(pid: number | undefined) {
  if (childPidFile && pid !== undefined) await writeFile(childPidFile, String(pid));
}
let child = start();
if (pidFile) await writeFile(pidFile, String(process.pid));
await recordAgentPid(child.pid);
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
      await recordAgentPid(child.pid);
    } else break;
  } while (!stopping);
  process.exitCode = code;
} finally {
  await rm(directory, { recursive: true, force: true });
  if (childPidFile) await rm(childPidFile, { force: true });
  if (pidFile) await rm(pidFile, { force: true });
}
