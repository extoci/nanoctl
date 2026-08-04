import type { GenericQueryCtx, GenericMutationCtx } from "convex/server";
import { ConvexError } from "convex/values";
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
  let value: unknown;
  try {
    value = JSON.parse(capabilitiesJson);
  } catch {
    return false;
  }
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const capabilities = value as Record<string, unknown>;
  return (
    capabilities.ready === true &&
    capabilities.protocolVersion === 1 &&
    Array.isArray(capabilities.codecs) &&
    capabilities.codecs.includes("h264") &&
    parseDeviceDisplays(capabilitiesJson).length > 0
  );
}

export type SignalKind = "offer" | "answer" | "ice-candidate" | "ice-complete" | "end";

export function parseSignalEnvelope(
  value: string,
  expectedSender: "controller" | "host",
): {
  sessionId: string;
  sender: "controller" | "host";
  sequence: number;
  kind: SignalKind;
  reason?: string;
} {
  let parsed: unknown;
  try {
    parsed = JSON.parse(value);
  } catch {
    throw new ConvexError("Malformed signal envelope");
  }
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
    throw new ConvexError("Malformed signal envelope");
  }
  const envelope = parsed as Record<string, unknown>;
  const payload = envelope.payload;
  if (!payload || typeof payload !== "object" || Array.isArray(payload)) {
    throw new ConvexError("Malformed signal payload");
  }
  const kind = (payload as Record<string, unknown>).type;
  const fields = payload as Record<string, unknown>;
  const permitted =
    expectedSender === "controller"
      ? ["offer", "ice-candidate", "ice-complete", "end"]
      : ["answer", "ice-candidate", "ice-complete", "end"];
  if (
    envelope.version !== 1 ||
    typeof envelope.sessionId !== "string" ||
    envelope.sender !== expectedSender ||
    !Number.isSafeInteger(envelope.sequence) ||
    Number(envelope.sequence) < 0 ||
    !Number.isSafeInteger(envelope.sentAt) ||
    Number(envelope.sentAt) <= 0 ||
    typeof kind !== "string" ||
    !permitted.includes(kind)
  ) {
    throw new ConvexError("Malformed signal envelope");
  }
  if (
    ((kind === "offer" || kind === "answer") && !isBoundedString(fields.sdp, 1, 1_000_000)) ||
    (kind === "ice-candidate" &&
      (!isBoundedString(fields.candidate, 1, 8_192) ||
        (fields.sdpMid !== null && !isBoundedString(fields.sdpMid, 0, 256)) ||
        (fields.sdpMLineIndex !== null &&
          (!Number.isInteger(fields.sdpMLineIndex) ||
            Number(fields.sdpMLineIndex) < 0 ||
            Number(fields.sdpMLineIndex) > 65_535)) ||
        (fields.usernameFragment !== undefined &&
          fields.usernameFragment !== null &&
          !isBoundedString(fields.usernameFragment, 1, 256)))) ||
    (kind === "end" && !isBoundedString(fields.reason, 1, 512))
  ) {
    throw new ConvexError("Malformed signal payload");
  }
  return {
    sessionId: envelope.sessionId,
    sender: expectedSender,
    sequence: Number(envelope.sequence),
    kind: kind as SignalKind,
    reason: kind === "end" ? String(fields.reason) : undefined,
  };
}

function isBoundedString(value: unknown, min: number, max: number): value is string {
  return typeof value === "string" && value.length >= min && value.length <= max;
}
