import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, readdirSync, realpathSync, statSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";

export function runtimeBytes(paths: string[]): number {
  const seen = new Set<string>();
  const visit = (path: string): number => {
    const canonical = realpathSync(path);
    if (seen.has(canonical)) return 0;
    seen.add(canonical);
    const stat = statSync(canonical);
    return stat.isDirectory()
      ? readdirSync(canonical).reduce((sum, name) => sum + visit(join(canonical, name)), 0)
      : stat.size;
  };
  return paths.reduce((sum, path) => sum + visit(path), 0);
}

export function libraryPaths(output: string): string[] {
  if (/\bnot found\b/.test(output)) throw new Error("Missing runtime library");
  return [...new Set([...output.matchAll(/(?:=>\s*)?(\/[^\s]+)/g)].map((match) => match[1]))];
}

if (import.meta.main) {
  const args = process.argv.slice(2);
  const index = args.indexOf("--container");
  const container = index < 0 ? null : args.splice(index, 2)[1];
  const [output, ...roots] = args;
  if (!output || !roots.length || (index >= 0 && !container))
    throw new Error(
      "Usage: bun scripts/bench/runtime.ts [--container NAME] OUTPUT_DIRECTORY ROOT...",
    );
  if (existsSync(output) || existsSync(`${output}.json`))
    throw new Error(`Refusing to overwrite ${output}`);
  const probe = `
    const fs = require('node:fs'), cp = require('node:child_process');
    const roots = JSON.parse(process.argv[1]);
    const libraries = new Set();
    for (const root of roots) {
      if (!fs.statSync(root).isFile()) continue;
      const fd = fs.openSync(root, 'r'), magic = Buffer.alloc(4);
      try { fs.readSync(fd, magic, 0, 4, 0); } finally { fs.closeSync(fd); }
      if (magic.toString('hex') !== '7f454c46') continue;
      let output;
      try { output = cp.execFileSync('ldd', [root], { encoding: 'utf8' }); }
      catch (error) {
        if (/not a dynamic executable|statically linked/.test(String(error.stdout) + String(error.stderr))) continue;
        throw error;
      }
      for (const path of (${libraryPaths.toString()})(output)) libraries.add(fs.realpathSync(path));
    }
    console.log(JSON.stringify({ roots: roots.map(p => fs.realpathSync(p)), libraries: [...libraries] }));
  `;
  const command = container ? "docker" : process.execPath;
  const parameters = container
    ? ["exec", container, "node", "-e", probe, JSON.stringify(roots)]
    : ["-e", probe, JSON.stringify(roots)];
  const manifest = JSON.parse(execFileSync(command, parameters, { encoding: "utf8" })) as {
    roots: string[];
    libraries: string[];
  };
  mkdirSync(output, { recursive: true });
  for (const source of new Set([...manifest.roots, ...manifest.libraries])) {
    const destination = join(resolve(output), source.slice(1));
    mkdirSync(dirname(destination), { recursive: true });
    if (container) execFileSync("docker", ["cp", "-L", `${container}:${source}`, destination]);
    else execFileSync("cp", ["-aL", source, destination]);
  }
  const bytes = runtimeBytes([output]);
  writeFileSync(
    `${output}.json`,
    JSON.stringify(
      {
        container,
        ...manifest,
        bytes,
        includes:
          "Explicit roots plus transitive ELF libraries; logical file bytes with symlink aliases deduplicated",
        excludes: "Other OS files, Xvfb, browsers, build toolchain and caches",
      },
      null,
      2,
    ),
  );
  console.log(`${bytes} bytes (${(bytes / 1048576).toFixed(2)} MiB): ${output}`);
}
