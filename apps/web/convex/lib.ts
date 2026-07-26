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

export type SignalKind = "offer" | "answer" | "ice-candidate" | "ice-complete" | "end";

export function parseSignalEnvelope(
  value: string,
  expectedSender: "controller" | "host",
): { sessionId: string; sender: "controller" | "host"; sequence: number; kind: SignalKind } {
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
    typeof kind !== "string" ||
    !permitted.includes(kind)
  ) {
    throw new ConvexError("Malformed signal envelope");
  }
  return {
    sessionId: envelope.sessionId,
    sender: expectedSender,
    sequence: Number(envelope.sequence),
    kind: kind as SignalKind,
  };
}
