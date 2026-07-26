import { ConvexError, v } from "convex/values";
import { mutation, query } from "./_generated/server";
import { parseSignalEnvelope, requireIdentity } from "./lib";
import { consumeRateLimit } from "./rateLimits";

const MAX_ENVELOPE_BYTES = 1_100_000;

export const list = query({
  args: { sessionId: v.id("sessions"), afterSequence: v.number() },
  handler: async (ctx, args) => {
    const identity = await requireIdentity(ctx);
    const session = await ctx.db.get(args.sessionId);
    if (!session || session.ownerId !== identity.subject)
      throw new ConvexError("Session not found");
    const latest = await ctx.db
      .query("signals")
      .withIndex("by_session_sender_sequence", (q) =>
        q.eq("sessionId", args.sessionId).eq("sender", "host").gt("sequence", args.afterSequence),
      )
      .order("desc")
      .take(127);
    const answer = await ctx.db
      .query("signals")
      .withIndex("by_session_sender_kind_sequence", (q) =>
        q.eq("sessionId", args.sessionId).eq("sender", "host").eq("kind", "answer"),
      )
      .first();
    return [...new Map([answer, ...latest].filter(Boolean).map((row) => [row!._id, row!])).values()]
      .filter((row) => row.sequence > args.afterSequence)
      .sort((left, right) => left.sequence - right.sequence);
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
      session.state === "ended" ||
      session.state === "failed"
    ) {
      throw new ConvexError("Session unavailable");
    }
    if (!(await consumeRateLimit(ctx, `signal:controller:${args.sessionId}`, 512, 60 * 1000))) {
      throw new ConvexError("Signal rate exceeded");
    }
    if (args.envelope.length > MAX_ENVELOPE_BYTES) throw new ConvexError("Signal too large");
    const parsed = parseSignalEnvelope(args.envelope, "controller");
    if (parsed.sessionId !== String(args.sessionId)) {
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
      kind: parsed.kind,
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
