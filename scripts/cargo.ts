const args = process.argv.slice(2);
if (args.length === 0) throw new Error("usage: bun scripts/cargo.ts <cargo arguments>");

const subprocess = Bun.spawn(
  ["cargo", ...args, "--manifest-path", "crates/nanoctl-agent/Cargo.toml"],
  {
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
  },
);

process.exit(await subprocess.exited);
