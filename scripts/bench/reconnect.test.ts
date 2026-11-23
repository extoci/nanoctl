import { expect, test } from "bun:test";
import { spawnSync } from "node:child_process";

function run(args: string[]) {
  return spawnSync(process.execPath, [`${import.meta.dir}/reconnect.ts`, ...args], {
    encoding: "utf8",
    env: { ...process.env, CYCLES: "20", MOTION_MS: "10000", SETTLE_MS: "3000" },
  });
}

test("reconnect probe rejects empty and invalid arguments before opening browsers", () => {
  for (const args of [[], ["0", "unused.json"], ["not-a-pid", "unused.json"]]) {
    const result = run(args);
    expect(result.status).not.toBe(0);
    expect(result.stderr).toContain("Usage:");
  }
});

test("reconnect probe refuses to overwrite existing artifacts", () => {
  const result = run(["1", import.meta.path]);
  expect(result.status).not.toBe(0);
  expect(result.stderr).toContain("Refusing to overwrite");
});
