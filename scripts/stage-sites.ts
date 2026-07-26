import { cp, mkdir, rename, rm } from "node:fs/promises";
import { resolve } from "node:path";

const repository = resolve(import.meta.dir, "..");
const output = resolve(repository, "dist");
const openNext = resolve(repository, "apps/web/.open-next");
const server = resolve(output, "server");

await rm(output, { recursive: true, force: true });
await mkdir(output, { recursive: true });
await cp(openNext, server, { recursive: true });
await rename(resolve(server, "worker.js"), resolve(server, "index.js"));
await rename(resolve(server, "assets"), resolve(output, "client"));

console.log("Staged OpenNext worker and static assets in dist.");
