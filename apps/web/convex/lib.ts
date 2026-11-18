import type { GenericQueryCtx, GenericMutationCtx } from "convex/server";
import { ConvexError } from "convex/values";
import { normalizeSignalEnvelope } from "@nanoctl/protocol";
import type { ReadinessState, SupportedProtocolVersion } from "@nanoctl/protocol";
import type { DataModel } from "./_generated/dataModel";

export async function requireIdentity(
  ctx: GenericQueryCtx<DataModel> | GenericMutationCtx<DataModel>,
) {
  const identity = await ctx.auth.getUserIdentity();
  if (!identity) throw new Error("Unauthenticated");
  return identity;
}

export function cleanDeviceName(value: string): string {
  const name = value.trim().replace(/\s+/g, " ");
  if (name.length < 1 || name.length > 80) throw new Error("Device name must be 1–80 characters");
  return name;
}

export function presenceWindowMs(heartbeatSeconds?: number): number {
  // Legacy agents keep 45s; slower agents get two intervals plus 15s of jitter.
  return Math.max(45_000, (heartbeatSeconds ?? 0) * 2_000 + 15_000);
}

export function assertActiveDeadline(expiresAt: number): void {
  if (expiresAt <= Date.now()) throw new Error("Expired");
}

export type DeviceDisplay = {
  id: string;
  name: string;
  width: number;
  height: number;
  scaleFactor: number;
  primary: boolean;
};

export function parseDeviceDisplays(capabilitiesJson: string): DeviceDisplay[] {
  let value: unknown;
  try {
    value = JSON.parse(capabilitiesJson);
  } catch {
    return [];
  }
  if (!value || typeof value !== "object" || Array.isArray(value)) return [];
  const displays = (value as Record<string, unknown>).displays;
  if (!Array.isArray(displays)) return [];
  return displays.slice(0, 16).flatMap((display) => {
    if (!display || typeof display !== "object" || Array.isArray(display)) return [];
    const row = display as Record<string, unknown>;
    if (
      typeof row.id !== "string" ||
      row.id.length < 1 ||
      row.id.length > 128 ||
      typeof row.name !== "string" ||
      row.name.length < 1 ||
      row.name.length > 256 ||
      typeof row.width !== "number" ||
      !Number.isInteger(row.width) ||
      row.width < 1 ||
      row.width > 16_384 ||
      typeof row.height !== "number" ||
      !Number.isInteger(row.height) ||
      row.height < 1 ||
      row.height > 16_384 ||
      typeof row.scaleFactor !== "number" ||
      !Number.isFinite(row.scaleFactor) ||
      row.scaleFactor <= 0 ||
      row.scaleFactor > 8 ||
      typeof row.primary !== "boolean"
    ) {
      return [];
    }
    return [
      {
        id: row.id,
        name: row.name,
        width: row.width,
        height: row.height,
        scaleFactor: row.scaleFactor,
        primary: row.primary,
      },
    ];
  });
}

export function parseDeviceReadiness(capabilitiesJson: string): boolean {
  return parseDeviceReadinessState(capabilitiesJson) === "ready";
}

export function parseDeviceProtocolVersion(
  capabilitiesJson: string,
): SupportedProtocolVersion | null {
  let value: unknown;
  try {
    value = JSON.parse(capabilitiesJson);
  } catch {
    return null;
  }
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  const protocolVersion = (value as Record<string, unknown>).protocolVersion;
  return protocolVersion === 1 || protocolVersion === 2 ? protocolVersion : null;
}

export function parseDeviceRemoteInput(capabilitiesJson: string): boolean {
  let value: unknown;
  try {
    value = JSON.parse(capabilitiesJson);
  } catch {
    return false;
  }
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const capabilities = value as Record<string, unknown>;
  if (typeof capabilities.remoteInput === "boolean") return capabilities.remoteInput;
  return capabilities.input === true;
}

export function parseDeviceReadinessState(capabilitiesJson: string): ReadinessState {
  let value: unknown;
  try {
    value = JSON.parse(capabilitiesJson);
  } catch {
    return "starting";
  }
  if (!value || typeof value !== "object" || Array.isArray(value)) return "starting";
  const capabilities = value as Record<string, unknown>;
  const explicit = capabilities.readiness;
  if (capabilities.protocolVersion !== 2) return "upgrade-required";
  const hasH264 = Array.isArray(capabilities.codecs) && capabilities.codecs.includes("h264");
  const hasDisplay = parseDeviceDisplays(capabilitiesJson).length > 0;
  if (
    explicit === "capture-permission" ||
    explicit === "input-permission" ||
    explicit === "no-display" ||
    explicit === "encoder-unavailable" ||
    explicit === "starting"
  ) {
    return explicit;
  }
  if (explicit === "ready" && hasH264 && hasDisplay) return "ready";
  if (!hasDisplay) return "no-display";
  if (!hasH264) {
    return "encoder-unavailable";
  }
  if (capabilities.ready === true) return "ready";
  if (capabilities.input === false) return "input-permission";
  return "starting";
}

export type SignalKind = "offer" | "answer" | "ice-candidate" | "ice-complete" | "end";

export function parseSignalEnvelope(
  value: string,
  expectedSender: "controller" | "host",
): {
  version: SupportedProtocolVersion;
  sessionId: string;
  sender: "controller" | "host";
  controllerEpoch: string;
  hostEpoch: string;
  sentAt: number;
  sequence: number;
  generation: number;
  kind: SignalKind;
  reason?: string;
} {
  let raw: unknown;
  let parsed: ReturnType<typeof normalizeSignalEnvelope>;
  try {
    raw = JSON.parse(value);
    parsed = normalizeSignalEnvelope(raw);
  } catch {
    throw new ConvexError("Malformed signal envelope");
  }
  const version: SupportedProtocolVersion =
    raw !== null &&
    typeof raw === "object" &&
    !Array.isArray(raw) &&
    (raw as Record<string, unknown>).version === 1
      ? 1
      : 2;
  const kind = parsed.payload.type;
  const permitted =
    expectedSender === "controller"
      ? ["offer", "ice-candidate", "ice-complete", "end"]
      : ["answer", "ice-candidate", "ice-complete", "end"];
  if (parsed.sender !== expectedSender || !permitted.includes(kind)) {
    throw new ConvexError("Malformed signal envelope");
  }
  return {
    version,
    sessionId: parsed.sessionId,
    sender: expectedSender,
    controllerEpoch: parsed.controllerEpoch,
    hostEpoch: parsed.hostEpoch,
    sentAt: parsed.sentAt,
    sequence: parsed.sequence,
    generation: parsed.generation,
    kind: kind as SignalKind,
    reason: kind === "end" ? parsed.payload.reason : undefined,
  };
}
