import { expect, test } from "bun:test";
import { mkdtempSync, mkdirSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { libraryPaths, runtimeBytes } from "./runtime";

test("runtime byte counts deduplicate aliases and overlapping directories", () => {
  const root = mkdtempSync(join(tmpdir(), "nanoctl-runtime-"));
  try {
    mkdirSync(join(root, "empty"));
    writeFileSync(join(root, "binary"), "12345");
    symlinkSync("binary", join(root, "alias"));
    symlinkSync(".", join(root, "loop"));
    expect(runtimeBytes([root, join(root, "binary")])).toBe(5);
    expect(runtimeBytes([join(root, "empty")])).toBe(0);
    expect(runtimeBytes([])).toBe(0);
    expect(() => runtimeBytes([join(root, "missing")])).toThrow();
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("runtime dependencies include the loader, deduplicate libraries and reject missing ones", () => {
  expect(
    libraryPaths(
      "linux-vdso.so.1 (0)\nlibc.so.6 => /lib/libc.so.6 (0)\n/lib/ld.so (0)\n/lib/libc.so.6 (0)",
    ),
  ).toEqual(["/lib/libc.so.6", "/lib/ld.so"]);
  expect(libraryPaths("")).toEqual([]);
  expect(() => libraryPaths("libx264.so => not found")).toThrow("Missing runtime library");
});
