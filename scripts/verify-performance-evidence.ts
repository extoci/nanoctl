import { readFile } from "node:fs/promises";

const requiredTargets = [
  "linux-x64",
  "linux-arm64",
  "macos-x64",
  "macos-arm64",
  "windows-x64",
  "windows-arm64",
] as const;
const requiredProfiles = ["interactive-1080p60", "soak-1080p30"] as const;

type Evidence = {
  schema_version?: unknown;
  passed?: unknown;
  agent_version?: unknown;
  web_version?: unknown;
  target_os?: unknown;
  target_arch?: unknown;
  profile?: unknown;
  elapsed_seconds?: unknown;
  width?: unknown;
  height?: unknown;
  average_fps?: unknown;
  first_frame_milliseconds?: unknown;
  p95_frame_milliseconds?: unknown;
  p99_frame_milliseconds?: unknown;
  p95_input_to_photon_milliseconds?: unknown;
  rss_start_megabytes?: unknown;
  rss_end_megabytes?: unknown;
  rss_peak_megabytes?: unknown;
  capture_queue_peak?: unknown;
  encode_queue_peak?: unknown;
  monotonic_memory_growth?: unknown;
};

function normalizeArch(value: string): string {
  if (value === "x86_64") return "x64";
  if (value === "aarch64") return "arm64";
  return value;
}

function finiteAtLeast(value: unknown, minimum: number): boolean {
  return typeof value === "number" && Number.isFinite(value) && value >= minimum;
}

export function validatePerformanceEvidence(records: readonly unknown[]): string[] {
  const errors: string[] = [];
  const observed = new Set<string>();
  let agentVersion: string | undefined;
  let webVersion: string | undefined;

  records.forEach((unknownRecord, index) => {
    const label = `record ${index + 1}`;
    if (typeof unknownRecord !== "object" || unknownRecord === null) {
      errors.push(`${label}: expected a JSON object`);
      return;
    }
    const record = unknownRecord as Evidence;
    if (record.schema_version !== 1) errors.push(`${label}: unsupported schema_version`);
    if (record.passed !== true) errors.push(`${label}: performance run did not pass`);
    agentVersion = consistentVersion(
      errors,
      label,
      "agent_version",
      record.agent_version,
      agentVersion,
    );
    webVersion = consistentVersion(errors, label, "web_version", record.web_version, webVersion);
    if (typeof record.target_os !== "string" || typeof record.target_arch !== "string") {
      errors.push(`${label}: missing target identity`);
      return;
    }
    const target = `${record.target_os}-${normalizeArch(record.target_arch)}`;
    if (!requiredTargets.includes(target as never))
      errors.push(`${label}: unexpected target ${target}`);
    if (!requiredProfiles.includes(record.profile as never)) {
      errors.push(`${label}: unexpected profile ${String(record.profile)}`);
      return;
    }
    const profile = record.profile as (typeof requiredProfiles)[number];
    const identity = `${target}/${profile}`;
    if (observed.has(identity)) errors.push(`${label}: duplicate run ${identity}`);
    else observed.add(identity);

    const minimumSeconds = profile === "interactive-1080p60" ? 1_800 : 28_800;
    const minimumFps = profile === "interactive-1080p60" ? 54 : 27;
    if (!finiteAtLeast(record.elapsed_seconds, minimumSeconds)) {
      errors.push(`${label}: elapsed_seconds is below ${minimumSeconds}`);
    }
    if (!finiteAtLeast(record.width, 1_920) || !finiteAtLeast(record.height, 1_080)) {
      errors.push(`${label}: resolution is below 1920x1080`);
    }
    if (!finiteAtLeast(record.average_fps, minimumFps)) {
      errors.push(`${label}: average_fps is below ${minimumFps}`);
    }
    for (const field of [
      "first_frame_milliseconds",
      "p95_frame_milliseconds",
      "p99_frame_milliseconds",
      "p95_input_to_photon_milliseconds",
      "rss_start_megabytes",
      "rss_end_megabytes",
      "rss_peak_megabytes",
    ] as const) {
      if (!finiteAtLeast(record[field], 0)) errors.push(`${label}: invalid ${field}`);
    }
    if (
      !Number.isSafeInteger(record.capture_queue_peak) ||
      Number(record.capture_queue_peak) < 0 ||
      Number(record.capture_queue_peak) > 1
    ) {
      errors.push(`${label}: capture_queue_peak exceeds the one-frame bound`);
    }
    if (
      !Number.isSafeInteger(record.encode_queue_peak) ||
      Number(record.encode_queue_peak) < 0 ||
      Number(record.encode_queue_peak) > 1
    ) {
      errors.push(`${label}: encode_queue_peak exceeds the one-frame bound`);
    }
    if (record.monotonic_memory_growth !== false) {
      errors.push(`${label}: monotonic memory growth was not ruled out`);
    }
  });

  for (const target of requiredTargets) {
    for (const profile of requiredProfiles) {
      const identity = `${target}/${profile}`;
      if (!observed.has(identity)) errors.push(`missing run ${identity}`);
    }
  }
  return errors;
}

function consistentVersion(
  errors: string[],
  label: string,
  field: "agent_version" | "web_version",
  value: unknown,
  expected: string | undefined,
): string | undefined {
  if (typeof value !== "string" || value.length === 0) {
    errors.push(`${label}: missing ${field}`);
    return expected;
  }
  if (expected !== undefined && value !== expected) {
    errors.push(`${label}: ${field} differs from the other records`);
  }
  return expected ?? value;
}

async function main(): Promise<void> {
  const paths = Bun.argv.slice(2);
  if (paths.length === 0) {
    throw new Error("usage: bun run evidence:performance -- <performance-evidence.json>...");
  }
  const records = await Promise.all(
    paths.map(async (path) => JSON.parse(await readFile(path, "utf8")) as unknown),
  );
  const errors = validatePerformanceEvidence(records);
  if (errors.length > 0) {
    for (const error of errors) console.error(error);
    process.exitCode = 2;
    return;
  }
  console.log(`performance evidence: pass (${records.length} target/profile runs)`);
}

if (import.meta.main) await main();
