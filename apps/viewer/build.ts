import { mkdir, rm, writeFile } from "node:fs/promises";
import { relative, resolve } from "node:path";

const root = import.meta.dir;
const outdir = resolve(root, "dist");
await rm(outdir, { recursive: true, force: true });
await mkdir(outdir, { recursive: true });
const result = await Bun.build({
  entrypoints: [resolve(root, "src/main.tsx")],
  outdir,
  target: "browser",
  minify: true,
  naming: "assets/[name]-[hash].[ext]",
  define: { "process.env.NODE_ENV": JSON.stringify("production") },
});
if (!result.success) throw new AggregateError(result.logs, "Viewer build failed");
const script = result.outputs.find((output) => output.path.endsWith(".js"));
const style = result.outputs.find((output) => output.path.endsWith(".css"));
if (!script || !style) throw new Error("Viewer script or stylesheet missing");
const asset = (path: string) => "/" + relative(outdir, path).replaceAll("\\", "/");
await writeFile(
  resolve(outdir, "index.html"),
  `<!doctype html>
<html lang="en"><head><meta charset="UTF-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<meta name="color-scheme" content="dark"><meta name="theme-color" content="#0c111a">
<title>nanoctl · Remote desktop</title><link rel="icon" href="/favicon.svg" type="image/svg+xml">
<link rel="stylesheet" href="${asset(style.path)}"><script type="module" src="${asset(script.path)}"></script></head>
<body><div id="root"></div><noscript>nanoctl needs JavaScript to connect to this desktop.</noscript></body></html>`,
);
await writeFile(
  resolve(outdir, "favicon.svg"),
  '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32"><rect width="32" height="32" rx="8" fill="#0c111a"/><path d="M8 24V8h5l6 10V8h5v16h-5L13 14v10z" fill="#8ef0cb"/></svg>',
);
console.log(`Viewer built: index.html + ${result.outputs.length} hashed assets`);
