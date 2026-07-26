import { ConvexError, v } from "convex/values";
import { mutation } from "./_generated/server";
import { requireIdentity } from "./lib";

const SESSION_TTL_MS = 15 * 60 * 1000;

export const create = mutation({
  args: { deviceId: v.id("devices") },
  handler: async (ctx, args) => {
    const identity = await requireIdentity(ctx);
    const device = await ctx.db.get(args.deviceId);
    if (
      !device ||
      device.ownerId !== identity.subject ||
      device.status === "disabled" ||
      Date.now() - device.lastSeenAt > 45_000
    ) {
      throw new ConvexError("Device is unavailable");
    }
    const now = Date.now();
    const expiresAt = now + SESSION_TTL_MS;
    const sessionId = await ctx.db.insert("sessions", {
      ownerId: identity.subject,
      deviceId: args.deviceId,
      state: "requested",
      createdAt: now,
      updatedAt: now,
      expiresAt,
    });
    await ctx.db.insert("auditEvents", {
      ownerId: identity.subject,
      deviceId: args.deviceId,
      sessionId,
      action: "session.requested",
      createdAt: now,
    });
    return { sessionId, expiresAt };
  },
});

export const end = mutation({
  args: { sessionId: v.id("sessions"), reason: v.string() },
  handler: async (ctx, args) => {
    const identity = await requireIdentity(ctx);
    const session = await ctx.db.get(args.sessionId);
    if (!session || session.ownerId !== identity.subject)
      throw new ConvexError("Session not found");
    const now = Date.now();
    await ctx.db.patch(args.sessionId, {
      state: "ended",
      endedAt: now,
      updatedAt: now,
      endReason: args.reason.slice(0, 256),
    });
    return null;
  },
});
