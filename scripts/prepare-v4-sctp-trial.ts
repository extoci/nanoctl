import { cp, readFile, writeFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";

const [, , directory, minimumRto] = process.argv;
if (!directory) throw new Error("Pass a temporary directory for the SCTP trial");
if (minimumRto !== "100" && minimumRto !== "200") {
  throw new Error("Pass an SCTP timer trial value of 100 or 200 ms");
}

const metadata = Bun.spawnSync(
  [
    "cargo",
    "metadata",
    "--manifest-path",
    "crates/nanoctl-agent/Cargo.toml",
    "--format-version",
    "1",
    "--filter-platform",
    "x86_64-unknown-linux-gnu",
    "--features",
    "media",
    "--locked",
  ],
  { stderr: "inherit" },
);
if (metadata.exitCode !== 0) throw new Error("Could not resolve the locked media dependencies");

const { packages } = JSON.parse(metadata.stdout.toString()) as {
  packages: { name: string; version: string; source: string | null; manifest_path: string }[];
};
const dependency = packages.find(({ name }) => name === "webrtc-sctp");
if (dependency?.version !== "0.17.3" || !dependency.source?.startsWith("registry+")) {
  throw new Error("The SCTP trial expects the locked webrtc-sctp 0.17.3 registry package");
}

const copy = resolve(directory, "webrtc-sctp");
await cp(dirname(dependency.manifest_path), copy, {
  recursive: true,
  force: false,
  errorOnExist: true,
});
const timer = resolve(copy, "src/timer/rtx_timer.rs");
const source = await readFile(timer, "utf8");
const baseline = "pub(crate) const RTO_MIN: u64 = 1000;";
if (source.split(baseline).length !== 2) throw new Error("The SCTP timer definition has changed");
await writeFile(timer, source.replace(baseline, `pub(crate) const RTO_MIN: u64 = ${minimumRto};`));
await writeFile(
  resolve(directory, "sctp-trial.toml"),
  `[patch.crates-io]\nwebrtc-sctp = { path = ${JSON.stringify(copy)} }\n`,
);
