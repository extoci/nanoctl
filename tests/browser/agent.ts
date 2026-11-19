import { spawn } from "node:child_process";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { homedir } from "node:os";
import { join } from "node:path";

const cache = join(homedir(), ".cache");
await mkdir(cache, { recursive: true });
const directory = await mkdtemp(join(cache, "nanoctl-browser-"));
const config = join(directory, "config.toml");
const port = Number(process.env.NANOCTL_BROWSER_PORT);
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
encoder = "software"
max_width = 1280
max_height = 720
max_fps = 30
`,
);
const env = {
  ...process.env,
  NANOCTL_DEV_LOOPBACK_USER: "dev@example.com",
  XDG_DATA_HOME: directory,
  XDG_CONFIG_HOME: directory,
  ...(process.platform === "linux" && !process.env.CC ? { CC: "gcc" } : {}),
};
const child = spawn(
  "cargo",
  [
    "run",
    "--features",
    process.env.NANOCTL_BROWSER_MEDIA === "1" ? "media" : "rtc",
    "--manifest-path",
    "crates/nanoctl-agent/Cargo.toml",
    "--",
    "--config",
    config,
    "run",
  ],
  { env, stdio: "inherit" },
);
let stopping = false;
const stop = () => {
  stopping = true;
  child.kill("SIGINT");
};
process.on("SIGTERM", stop);
process.on("SIGINT", stop);
try {
  const code = await new Promise<number>((resolve, reject) => {
    child.once("error", reject);
    child.once("exit", (code) => resolve(code ?? (stopping ? 0 : 1)));
  });
  process.exitCode = code;
} finally {
  await rm(directory, { recursive: true, force: true });
}
