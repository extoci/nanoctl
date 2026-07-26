import { ConvexError, v } from "convex/values";
import { mutation, query } from "./_generated/server";
import { requireIdentity } from "./lib";

const MAX_ENVELOPE_BYTES = 1_100_000;

export const list = query({
  args: { sessionId: v.id("sessions"), afterSequence: v.number() },
  handler: async (ctx, args) => {
    const identity = await requireIdentity(ctx);
    const session = await ctx.db.get(args.sessionId);
    if (!session || session.ownerId !== identity.subject)
      throw new ConvexError("Session not found");
    return ctx.db
      .query("signals")
      .withIndex("by_session_sender_sequence", (q) =>
        q.eq("sessionId", args.sessionId).eq("sender", "host").gt("sequence", args.afterSequence),
      )
      .take(128);
  },
});

export const send = mutation({
  args: { sessionId: v.id("sessions"), envelope: v.string() },
  handler: async (ctx, args) => {
    const identity = await requireIdentity(ctx);
    const session = await ctx.db.get(args.sessionId);
    if (
      !session ||
      session.ownerId !== identity.subject ||
      session.expiresAt <= Date.now() ||
      session.state === "ended"
    ) {
      throw new ConvexError("Session unavailable");
    }
    if (args.envelope.length > MAX_ENVELOPE_BYTES) throw new ConvexError("Signal too large");
    const parsed = parseEnvelope(args.envelope);
    if (parsed.sessionId !== String(args.sessionId) || parsed.sender !== "controller") {
      throw new ConvexError("Invalid signal envelope");
    }
    const duplicate = await ctx.db
      .query("signals")
      .withIndex("by_session_sender_sequence", (q) =>
        q
          .eq("sessionId", args.sessionId)
          .eq("sender", "controller")
          .eq("sequence", parsed.sequence),
      )
      .unique();
    if (duplicate) return null;
    const now = Date.now();
    await ctx.db.insert("signals", {
      sessionId: args.sessionId,
      sender: "controller",
      sequence: parsed.sequence,
      envelope: args.envelope,
      createdAt: now,
      expiresAt: session.expiresAt,
    });
    if (session.state === "requested") {
      await ctx.db.patch(args.sessionId, { state: "negotiating", updatedAt: now });
    }
    return null;
  },
});

function parseEnvelope(value: string): { sessionId: string; sender: string; sequence: number } {
  let parsed: unknown;
  try {
    parsed = JSON.parse(value);
  } catch {
    throw new ConvexError("Malformed signal envelope");
  }
  if (!parsed || typeof parsed !== "object") throw new ConvexError("Malformed signal envelope");
  const envelope = parsed as Record<string, unknown>;
  if (
    envelope.version !== 1 ||
    typeof envelope.sessionId !== "string" ||
    typeof envelope.sender !== "string" ||
    !Number.isSafeInteger(envelope.sequence) ||
    Number(envelope.sequence) < 0
  ) {
    throw new ConvexError("Malformed signal envelope");
  }
  return {
    sessionId: envelope.sessionId,
    sender: envelope.sender,
    sequence: Number(envelope.sequence),
  };
}
