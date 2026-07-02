import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { chmodSync, existsSync, mkdirSync } from "node:fs";
import { homedir, platform } from "node:os";
import { dirname, join } from "node:path";
import type { ControlEventPayload, RuntimeConfig } from "./types";

type ControlBridge = {
  enabled: boolean;
  description: string;
  send: (event: ControlEventPayload) => void;
  shutdown: () => void;
};

function resolveGoBinary(): string | null {
  if (process.env.GO_BINARY?.trim()) {
    return process.env.GO_BINARY.trim();
  }

  const localGo = join(homedir(), ".local", "toolchains", "go", "bin", "go");
  if (existsSync(localGo)) {
    return localGo;
  }

  return "go";
}

function buildControlBridgeSync(targetPath: string): void {
  const sourceDir = join(import.meta.dir, "..", "go", "controlbridge");
  const goBinary = resolveGoBinary();
  if (!goBinary) {
    throw new Error("go toolchain not found");
  }
  mkdirSync(dirname(targetPath), { recursive: true });
  const build = Bun.spawnSync([goBinary, "build", "-o", targetPath, "."], {
    cwd: sourceDir,
    stderr: "pipe",
    stdout: "pipe",
  });
  if (build.exitCode !== 0) {
    const stderr = build.stderr.toString();
    throw new Error(`failed to build control bridge: ${stderr.trim()}`);
  }
  chmodSync(targetPath, 0o755);
}

function probeControlBridge(targetPath: string): string | null {
  const probe = Bun.spawnSync([targetPath, "--probe"], {
    stdout: "pipe",
    stderr: "pipe",
  });

  if (probe.exitCode === 0) {
    return null;
  }

  const stderr = probe.stderr.toString().trim();
  return stderr || "control bridge probe failed";
}

export function createControlBridge(config: RuntimeConfig): ControlBridge {
  if (!config.controlEnabled) {
    return {
      enabled: false,
      description: "Disabled by configuration",
      send() {},
      shutdown() {},
    };
  }

  if (platform() !== "linux") {
    return {
      enabled: false,
      description: "Control bridge currently supports Linux/X11 hosts only",
      send() {},
      shutdown() {},
    };
  }

  const bridgePath = config.controlBridgePath ?? join(process.cwd(), ".nanoctl", "controlbridge");
  const shouldBuildLocalBridge = !config.controlBridgePath;
  if (shouldBuildLocalBridge || !existsSync(bridgePath)) {
    try {
      buildControlBridgeSync(bridgePath);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      return {
        enabled: false,
        description: `Disabled: ${message}`,
        send() {},
        shutdown() {},
      };
    }
  }

  const probeError = probeControlBridge(bridgePath);
  if (probeError) {
    return {
      enabled: false,
      description: `Disabled: ${probeError}`,
      send() {},
      shutdown() {},
    };
  }

  const child: ChildProcessWithoutNullStreams = spawn(bridgePath, [], {
    stdio: ["pipe", "inherit", "pipe"],
  });
  child.stderr.setEncoding("utf8");
  child.stderr.on("data", (chunk) => {
    const text = chunk.trim();
    if (text) console.warn(`[control-bridge] ${text}`);
  });

  return {
    enabled: true,
    description: `Linux/X11 XTEST via ${bridgePath}`,
    send(event) {
      if (child.exitCode !== null) return;
      child.stdin.write(`${JSON.stringify(event)}\n`);
    },
    shutdown() {
      if (child.exitCode !== null) return;
      child.kill("SIGTERM");
    },
  };
}
