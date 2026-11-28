import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const args = process.argv.slice(2);
if (args.length === 0) throw new Error("usage: bun scripts/cargo.ts <cargo arguments>");
const separator = args.indexOf("--");
const manifestArgs = args.includes("--manifest-path")
  ? []
  : ["--manifest-path", "crates/nanoctl-agent/Cargo.toml"];
const cargoArgs =
  separator === -1
    ? [...args, ...manifestArgs]
    : [...args.slice(0, separator), ...manifestArgs, ...args.slice(separator)];

const cargoHome = process.env.CARGO_HOME;
const cargoFromHome = cargoHome
  ? join(cargoHome, "bin", process.platform === "win32" ? "cargo.exe" : "cargo")
  : undefined;
const cargo = cargoFromHome && existsSync(cargoFromHome) ? cargoFromHome : "cargo";

const subprocess = spawn(cargo, cargoArgs, {
  cwd: fileURLToPath(new URL("..", import.meta.url)),
  // Bun 1.3 on Windows cannot launch Rust's rustup proxy executables directly even when the
  // absolute path exists. Route that platform through ComSpec; other platforms stay direct.
  shell: process.platform === "win32",
  env: {
    ...process.env,
    // Some development shells expose `cc` as a cross-target wrapper. Prefer the conventional
    // compiler name for crates that build portable C/assembly sources.
    ...(process.platform === "linux" && !process.env.CC ? { CC: "gcc" } : {}),
  },
  stdio: "inherit",
});

const exitCode = await new Promise<number>((resolve, reject) => {
  subprocess.once("error", reject);
  subprocess.once("exit", (code, signal) => {
    if (signal) {
      reject(new Error(`cargo terminated by ${signal}`));
      return;
    }
    resolve(code ?? 1);
  });
});
process.exit(exitCode);
