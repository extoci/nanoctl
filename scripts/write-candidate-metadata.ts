import { basename, join, resolve } from "node:path";

type Platform = "linux" | "macos" | "windows";

function packageVersion(text: string): string {
  const parsed = JSON.parse(text) as { version?: unknown };
  if (typeof parsed.version !== "string" || !/^\d+\.\d+\.\d+$/.test(parsed.version)) {
    throw new Error("package.json must contain a stable semantic version");
  }
  return parsed.version;
}

function cargoVersion(text: string): string {
  const packageSection = text.match(/\[package\]([\s\S]*?)(?:\n\[|$)/)?.[1] ?? "";
  const version = packageSection.match(/^\s*version\s*=\s*"([^"]+)"\s*$/m)?.[1];
  if (!version || !/^\d+\.\d+\.\d+$/.test(version)) {
    throw new Error("agent Cargo.toml must contain a stable semantic version");
  }
  return version;
}

const [stageArgument, platformArgument, architecture] = process.argv.slice(2);
if (!stageArgument || !["linux", "macos", "windows"].includes(platformArgument ?? "")) {
  throw new Error("usage: write-candidate-metadata.ts STAGE (linux|macos|windows) ARCH");
}
if (!architecture || !["x64", "arm64"].includes(architecture)) {
  throw new Error("candidate architecture must be x64 or arm64");
}
const platform = platformArgument as Platform;
const repository = resolve(import.meta.dir, "..");
const stage = resolve(repository, stageArgument);
const binary = join(stage, platform === "windows" ? "nanoctl.exe" : "nanoctl");
if (!(await Bun.file(binary).exists())) throw new Error("candidate binary is missing");

const [rootPackage, agentManifest] = await Promise.all([
  Bun.file(join(repository, "package.json")).text(),
  Bun.file(join(repository, "crates/nanoctl-agent/Cargo.toml")).text(),
]);
const version = packageVersion(rootPackage);
if (cargoVersion(agentManifest) !== version) {
  throw new Error("web/repository and agent versions do not match");
}
if (process.env.GITHUB_REF_TYPE === "tag" && process.env.GITHUB_REF_NAME !== `v${version}`) {
  throw new Error(`release tag must be v${version}`);
}
const commit = process.env.GITHUB_SHA;
if (!commit || !/^[0-9a-f]{40}$/.test(commit)) {
  throw new Error("GITHUB_SHA must identify the exact candidate commit");
}
const rustVersion = process.env.NANOCTL_RUSTC_VERSION?.trim();
if (!rustVersion || !/^rustc \d+\.\d+\.\d+ \([0-9a-f]+ \d{4}-\d{2}-\d{2}\)$/.test(rustVersion)) {
  throw new Error("NANOCTL_RUSTC_VERSION must contain the exact candidate rustc version");
}

const digest = new Bun.CryptoHasher("sha256")
  .update(await Bun.file(binary).arrayBuffer())
  .digest("hex");
const signingRequirements: Record<Platform, string[]> = {
  linux: ["sign-package", "attest-signed-bytes"],
  macos: ["codesign-hardened-runtime", "notarize", "staple", "attest-signed-bytes"],
  windows: ["authenticode-binary", "authenticode-installer", "attest-signed-bytes"],
};
const metadata = {
  schemaVersion: 1,
  product: "nanoctl",
  version,
  commit,
  target: { platform, architecture },
  binary: {
    name: basename(binary),
    size: Bun.file(binary).size,
    sha256: digest,
  },
  toolchain: {
    bun: Bun.version,
    rust: rustVersion,
  },
  unsigned: true,
  signingRequirements: signingRequirements[platform],
};
await Bun.write(join(stage, "CANDIDATE.json"), `${JSON.stringify(metadata, null, 2)}\n`);
console.log(`Wrote unsigned ${platform}-${architecture} candidate metadata.`);
