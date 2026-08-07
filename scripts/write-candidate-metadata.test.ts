import { afterEach, describe, expect, test } from "bun:test";
import { chmod, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const repository = resolve(import.meta.dir, "..");
const temporaryDirectories: string[] = [];
const commit = "a".repeat(40);
const rustVersion = "rustc 1.96.1 (31fca3adb 2026-06-26)";

afterEach(async () => {
  await Promise.all(
    temporaryDirectories
      .splice(0)
      .map((directory) => rm(directory, { recursive: true, force: true })),
  );
});

async function fixture(): Promise<string> {
  const stage = await mkdtemp(join(tmpdir(), "nanoctl-candidate-"));
  temporaryDirectories.push(stage);
  const binary = join(stage, "nanoctl");
  await writeFile(binary, "#!/bin/sh\nexit 0\n");
  await chmod(binary, 0o755);
  return stage;
}

function generate(stage: string, environment: Record<string, string>): Bun.Subprocess {
  return Bun.spawn(["bun", "run", "scripts/write-candidate-metadata.ts", stage, "linux", "x64"], {
    cwd: repository,
    env: {
      ...process.env,
      GITHUB_SHA: commit,
      NANOCTL_RUSTC_VERSION: rustVersion,
      ...environment,
    },
    stdout: "pipe",
    stderr: "pipe",
  });
}

describe("release candidate identity", () => {
  test("records exact version, commit, target, digest, and unsigned state", async () => {
    const stage = await fixture();
    const child = generate(stage, {});
    expect(await child.exited).toBe(0);
    const metadata = (await Bun.file(join(stage, "CANDIDATE.json")).json()) as {
      version: string;
      commit: string;
      target: { platform: string; architecture: string };
      binary: { size: number; sha256: string };
      unsigned: boolean;
      signingRequirements: string[];
    };
    expect(metadata).toMatchObject({
      version: "1.0.19",
      commit,
      target: { platform: "linux", architecture: "x64" },
      binary: { size: 17 },
      unsigned: true,
    });
    expect(metadata.binary.sha256).toMatch(/^[0-9a-f]{64}$/);
    expect(metadata.signingRequirements).toContain("attest-signed-bytes");
  });

  test("rejects a tag that disagrees with the package version", async () => {
    const stage = await fixture();
    const child = generate(stage, {
      GITHUB_REF_TYPE: "tag",
      GITHUB_REF_NAME: "v9.9.9",
    });
    expect(await child.exited).not.toBe(0);
    expect(await Bun.file(join(stage, "CANDIDATE.json")).exists()).toBe(false);
  });
});
