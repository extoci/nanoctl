export type EnrollmentInput = {
  code: string;
  name: string;
  platform: "windows" | "macos" | "linux";
  architecture: "x64" | "arm64";
  agentVersion: string;
  capabilitiesJson: string;
};

export function parseEnrollmentInput(body: Record<string, unknown>): EnrollmentInput | null {
  if (
    typeof body.code !== "string" ||
    typeof body.name !== "string" ||
    !isPlatform(body.platform) ||
    !isArchitecture(body.architecture) ||
    typeof body.agentVersion !== "string" ||
    !isRecord(body.capabilities)
  ) {
    return null;
  }
  const code = body.code.trim().toUpperCase();
  const name = body.name.trim().replace(/\s+/g, " ");
  const agentVersion = body.agentVersion.trim();
  const capabilitiesJson = boundedJson(body.capabilities, 64_000);
  if (
    !/^[0-9ABCDEFGHJKMNPQRSTVWXYZ]{5}(?:-[0-9ABCDEFGHJKMNPQRSTVWXYZ]{5}){3}$/.test(code) ||
    name.length < 1 ||
    name.length > 80 ||
    agentVersion.length < 1 ||
    agentVersion.length > 32 ||
    capabilitiesJson === null
  ) {
    return null;
  }
  return {
    code,
    name,
    platform: body.platform,
    architecture: body.architecture,
    agentVersion,
    capabilitiesJson,
  };
}

export function boundedCapabilities(value: unknown): string | null {
  return boundedJson(value ?? {}, 64_000);
}

function boundedJson(value: unknown, maxBytes: number): string | null {
  let serialized: string | undefined;
  try {
    serialized = JSON.stringify(value);
  } catch {
    return null;
  }
  if (serialized === undefined || new TextEncoder().encode(serialized).byteLength > maxBytes) {
    return null;
  }
  return serialized;
}

function isPlatform(value: unknown): value is "windows" | "macos" | "linux" {
  return value === "windows" || value === "macos" || value === "linux";
}

function isArchitecture(value: unknown): value is "x64" | "arm64" {
  return value === "x64" || value === "arm64";
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}
