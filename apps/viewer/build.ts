import { copyFile, mkdir, rm, writeFile } from "node:fs/promises";
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
<html lang="en"><head><meta charset="UTF-8"><meta name="viewport" content="width=device-width,initial-scale=1,interactive-widget=resizes-content,viewport-fit=cover">
<meta name="color-scheme" content="dark"><meta name="theme-color" content="#0c111a">
<meta name="apple-mobile-web-app-capable" content="yes"><meta name="apple-mobile-web-app-title" content="nanoctl">
<title>nanoctl · Remote desktop</title><link rel="icon" href="/favicon.svg" type="image/svg+xml">
<link rel="apple-touch-icon" href="/app-icon.png"><link rel="manifest" href="/manifest.webmanifest">
<link rel="stylesheet" href="${asset(style.path)}"><script type="module" src="${asset(script.path)}"></script></head>
<body><div id="root"></div><noscript>nanoctl needs JavaScript to connect to this desktop.</noscript></body></html>`,
);
await copyFile(resolve(root, "src/favicon.svg"), resolve(outdir, "favicon.svg"));
await copyFile(resolve(root, "src/app-icon.png"), resolve(outdir, "app-icon.png"));
await writeFile(
  resolve(outdir, "manifest.webmanifest"),
  JSON.stringify({
    name: "nanoctl Remote Desktop",
    short_name: "nanoctl",
    start_url: "/",
    display: "standalone",
    background_color: "#0c111a",
    theme_color: "#0c111a",
    icons: [{ src: "/app-icon.png", sizes: "192x192", type: "image/png" }],
  }),
);
console.log(`Viewer built: index.html + ${result.outputs.length} hashed assets`);
