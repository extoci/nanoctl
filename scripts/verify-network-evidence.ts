import { readFile } from "node:fs/promises";

export const requiredNetworkScenarios = [
  "same-lan",
  "double-nat",
  "symmetric-nat",
  "ipv4-only",
  "ipv6-only",
  "cgnat",
  "udp-blocked",
  "tcp-turn",
  "tls-turn-443",
  "packet-loss-1",
  "packet-loss-10",
  "rtt-200",
  "bandwidth-step",
  "interface-switch",
  "outage-5s",
  "turn-loss",
] as const;

type Evidence = {
  schema_version?: unknown;
  passed?: unknown;
  agent_version?: unknown;
  web_version?: unknown;
  scenario?: unknown;
  duration_seconds?: unknown;
  setup_milliseconds?: unknown;
  recovery_milliseconds?: unknown;
  samples?: unknown;
  selected_route?: unknown;
  average_bitrate_kbps?: unknown;
  p95_rtt_milliseconds?: unknown;
  p95_input_to_photon_milliseconds?: unknown;
  packets_lost?: unknown;
  frames_dropped?: unknown;
};

function finiteAtLeast(value: unknown, minimum: number): boolean {
  return typeof value === "number" && Number.isFinite(value) && value >= minimum;
}

export function validateNetworkEvidence(
  records: readonly unknown[],
  minimumDurationSeconds = 300,
): string[] {
  const errors: string[] = [];
  const observed = new Set<string>();
  let expectedAgentVersion: string | undefined;
  let expectedWebVersion: string | undefined;

  records.forEach((unknownRecord, index) => {
    const label = `record ${index + 1}`;
    if (typeof unknownRecord !== "object" || unknownRecord === null) {
      errors.push(`${label}: expected a JSON object`);
      return;
    }
    const record = unknownRecord as Evidence;
    if (record.schema_version !== 1) errors.push(`${label}: unsupported schema_version`);
    if (record.passed !== true) errors.push(`${label}: scenario did not pass`);
    expectedAgentVersion = validateConsistentVersion(
      errors,
      label,
      "agent_version",
      record.agent_version,
      expectedAgentVersion,
    );
    expectedWebVersion = validateConsistentVersion(
      errors,
      label,
      "web_version",
      record.web_version,
      expectedWebVersion,
    );
    if (
      typeof record.duration_seconds !== "number" ||
      record.duration_seconds < minimumDurationSeconds
    ) {
      errors.push(`${label}: duration_seconds is below ${minimumDurationSeconds}`);
    }
    if (!finiteAtLeast(record.setup_milliseconds, 1)) {
      errors.push(`${label}: invalid setup_milliseconds`);
    }
    for (const field of [
      "recovery_milliseconds",
      "average_bitrate_kbps",
      "p95_rtt_milliseconds",
      "p95_input_to_photon_milliseconds",
      "packets_lost",
      "frames_dropped",
    ] as const) {
      if (!finiteAtLeast(record[field], 0)) errors.push(`${label}: invalid ${field}`);
    }
    if (!Number.isSafeInteger(record.samples) || Number(record.samples) < 1) {
      errors.push(`${label}: invalid samples`);
    }
    if (!["direct", "relay", "mixed"].includes(String(record.selected_route))) {
      errors.push(`${label}: invalid selected_route`);
    }
    if (typeof record.scenario !== "string") {
      errors.push(`${label}: missing scenario`);
      return;
    }
    if (!requiredNetworkScenarios.includes(record.scenario as never)) {
      errors.push(`${label}: unexpected scenario ${record.scenario}`);
    } else if (observed.has(record.scenario)) {
      errors.push(`${label}: duplicate scenario ${record.scenario}`);
    } else {
      observed.add(record.scenario);
    }
  });

  for (const scenario of requiredNetworkScenarios) {
    if (!observed.has(scenario)) errors.push(`missing scenario ${scenario}`);
  }
  return errors;
}

function validateConsistentVersion(
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
    throw new Error("usage: bun run evidence:network -- <network-evidence.json>...");
  }
  const records = await Promise.all(
    paths.map(async (path) => JSON.parse(await readFile(path, "utf8")) as unknown),
  );
  const errors = validateNetworkEvidence(records);
  if (errors.length > 0) {
    for (const error of errors) console.error(error);
    process.exitCode = 2;
    return;
  }
  console.log(`network evidence: pass (${records.length} scenarios)`);
}

if (import.meta.main) {
  await main();
}
