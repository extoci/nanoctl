import { readFile } from "node:fs/promises";

const requiredTargets = [
  "linux-x64",
  "linux-arm64",
  "macos-x64",
  "macos-arm64",
  "windows-x64",
  "windows-arm64",
] as const;

type Evidence = {
  schema_version?: unknown;
  passed?: unknown;
  agent_version?: unknown;
  target_os?: unknown;
  target_arch?: unknown;
  backend?: unknown;
  requested_seconds?: unknown;
  elapsed_milliseconds?: unknown;
  frames?: unknown;
  encoded_bytes?: unknown;
  width?: unknown;
  height?: unknown;
  idr_frames?: unknown;
  sps_units?: unknown;
  pps_units?: unknown;
};

function normalizeArch(value: string): string {
  if (value === "x86_64") return "x64";
  if (value === "aarch64") return "arm64";
  return value;
}

function positiveNumber(record: Evidence, field: keyof Evidence): boolean {
  const value = record[field];
  return typeof value === "number" && Number.isFinite(value) && value > 0;
}

export function validateMediaEvidence(
  records: readonly unknown[],
  minimumSeconds = 1_800,
): string[] {
  const errors: string[] = [];
  const observed = new Set<string>();
  let expectedVersion: string | undefined;

  records.forEach((unknownRecord, index) => {
    const label = `record ${index + 1}`;
    if (typeof unknownRecord !== "object" || unknownRecord === null) {
      errors.push(`${label}: expected a JSON object`);
      return;
    }
    const record = unknownRecord as Evidence;
    if (record.schema_version !== 1) errors.push(`${label}: unsupported schema_version`);
    if (record.passed !== true) errors.push(`${label}: media smoke did not pass`);
    if (typeof record.agent_version !== "string" || record.agent_version.length === 0) {
      errors.push(`${label}: missing agent_version`);
    } else if (expectedVersion === undefined) {
      expectedVersion = record.agent_version;
    } else if (record.agent_version !== expectedVersion) {
      errors.push(`${label}: agent_version differs from the other records`);
    }
    if (typeof record.requested_seconds !== "number" || record.requested_seconds < minimumSeconds) {
      errors.push(`${label}: requested_seconds is below ${minimumSeconds}`);
    }
    for (const field of [
      "elapsed_milliseconds",
      "frames",
      "encoded_bytes",
      "width",
      "height",
      "idr_frames",
      "sps_units",
      "pps_units",
    ] as const) {
      if (!positiveNumber(record, field)) errors.push(`${label}: invalid ${field}`);
    }
    if (record.backend === "OpenH264" || typeof record.backend !== "string") {
      errors.push(`${label}: hardware encoder evidence is absent`);
    }
    if (typeof record.target_os !== "string" || typeof record.target_arch !== "string") {
      errors.push(`${label}: missing target identity`);
      return;
    }
    const target = `${record.target_os}-${normalizeArch(record.target_arch)}`;
    if (!requiredTargets.includes(target as (typeof requiredTargets)[number])) {
      errors.push(`${label}: unexpected target ${target}`);
    } else if (observed.has(target)) {
      errors.push(`${label}: duplicate target ${target}`);
    } else {
      observed.add(target);
    }
  });

  for (const target of requiredTargets) {
    if (!observed.has(target)) errors.push(`missing target ${target}`);
  }
  return errors;
}

async function main(): Promise<void> {
  const paths = Bun.argv.slice(2);
  if (paths.length === 0) {
    throw new Error("usage: bun run evidence:media -- <media-smoke.json>...");
  }
  const records = await Promise.all(
    paths.map(async (path) => JSON.parse(await readFile(path, "utf8")) as unknown),
  );
  const errors = validateMediaEvidence(records);
  if (errors.length > 0) {
    for (const error of errors) console.error(error);
    process.exitCode = 2;
    return;
  }
  console.log(`media evidence: pass (${records.length} physical targets)`);
}

if (import.meta.main) {
  await main();
}
