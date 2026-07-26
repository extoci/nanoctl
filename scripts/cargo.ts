import { existsSync } from "node:fs";
import { join } from "node:path";

const args = process.argv.slice(2);
if (args.length === 0) throw new Error("usage: bun scripts/cargo.ts <cargo arguments>");
const separator = args.indexOf("--");
const cargoArgs =
  separator === -1
    ? [...args, "--manifest-path", "crates/nanoctl-agent/Cargo.toml"]
    : [
        ...args.slice(0, separator),
        "--manifest-path",
        "crates/nanoctl-agent/Cargo.toml",
        ...args.slice(separator),
      ];

const cargoHome = process.env.CARGO_HOME;
const cargoFromHome = cargoHome
  ? join(cargoHome, "bin", process.platform === "win32" ? "cargo.exe" : "cargo")
  : undefined;
const cargo = cargoFromHome && existsSync(cargoFromHome) ? cargoFromHome : "cargo";

const subprocess = Bun.spawn([cargo, ...cargoArgs], {
  cwd: new URL("..", import.meta.url).pathname,
  env: {
    ...process.env,
    // Some development shells expose `cc` as a cross-target wrapper. Prefer the conventional
    // compiler name for crates that build portable C/assembly sources.
    ...(process.platform === "linux" && !process.env.CC ? { CC: "gcc" } : {}),
  },
  stdin: "inherit",
  stdout: "inherit",
  stderr: "inherit",
});

process.exit(await subprocess.exited);
