import { rm } from "node:fs/promises";

const generatedPaths = ["apps/web/.next", "packages/protocol/dist", "target", "coverage"];

await Promise.all(
  generatedPaths.map(async (path) => {
    await rm(path, { recursive: true, force: true });
  }),
);
