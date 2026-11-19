import { rm } from "node:fs/promises";

const generatedPaths = [
  "apps/viewer/dist",
  "packages/protocol/dist",
  "target",
  "coverage",
  "playwright-report",
  "test-results",
];

await Promise.all(
  generatedPaths.map(async (path) => {
    await rm(path, { recursive: true, force: true });
  }),
);
