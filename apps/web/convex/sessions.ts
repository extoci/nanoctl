import { ConvexError, v } from "convex/values";
import { action, internalQuery, mutation, query } from "./_generated/server";
import { internal } from "./_generated/api";
import {
  parseDeviceDisplays,
  parseDeviceReadiness,
  parseDeviceRemoteInput,
  requireIdentity,
} from "./lib";
import { consumeRateLimit } from "./rateLimits";
import { mintTurnCredentials } from "./turn";

const SESSION_TTL_MS = 15 * 60 * 1000;

export const getState = query({
  args: { sessionId: v.id("sessions") },
  handler: async (ctx, args) => {
    const identity = await requireIdentity(ctx);
    const session = await ctx.db.get(args.sessionId);
    if (!session || session.ownerId !== identity.subject)
      throw new ConvexError("Session not found");
    const device = await ctx.db.get(session.deviceId);
    const expired =
      session.expiresAt <= Date.now() && session.state !== "ended" && session.state !== "failed";
    return {
      state: expired ? ("failed" as const) : session.state,
      expiresAt: session.expiresAt,
      endReason: expired ? "session expired" : session.endReason,
      controllerEpoch: session.controllerEpoch,
      remoteInput: device ? parseDeviceRemoteInput(device.capabilitiesJson) : false,
      displays: device ? parseDeviceDisplays(device.capabilitiesJson) : [],
    };
  },
});

export const create = mutation({
  args: { deviceId: v.id("devices") },
  handler: async (ctx, args) => {
    const identity = await requireIdentity(ctx);
    if (!(await consumeRateLimit(ctx, `session:${identity.subject}`, 30, 60 * 60 * 1000))) {
      throw new ConvexError("Too many session requests");
    }
    const device = await ctx.db.get(args.deviceId);
    if (
      !device ||
      device.ownerId !== identity.subject ||
      device.status === "disabled" ||
      !parseDeviceReadiness(device.capabilitiesJson) ||
      Date.now() - device.lastSeenAt > 45_000
    ) {
      throw new ConvexError("Device is unavailable");
    }
    for (const state of ["requested", "ringing", "negotiating", "connected"] as const) {
      const existing = await ctx.db
        .query("sessions")
        .withIndex("by_device_state", (q) => q.eq("deviceId", args.deviceId).eq("state", state))
        .first();
      if (existing && existing.expiresAt > Date.now()) {
        throw new ConvexError("Device already has an active session");
      }
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

/**
 * Claim the controller role for one browser peer. A refresh intentionally creates a new claim;
 * every later controller signal must carry the currently stored epoch, so delayed rows from the
 * previous page cannot become authoritative merely because they arrived later.
 */
export const claimController = mutation({
  args: {
    sessionId: v.id("sessions"),
    controllerEpoch: v.string(),
    expectedControllerEpoch: v.optional(v.string()),
  },
  handler: async (ctx, args) => {
    const identity = await requireIdentity(ctx);
    if (
      args.controllerEpoch.length < 1 ||
      args.controllerEpoch.length > 128 ||
      (args.expectedControllerEpoch !== undefined &&
        (args.expectedControllerEpoch.length < 1 || args.expectedControllerEpoch.length > 128))
    ) {
      throw new ConvexError("Invalid controller claim");
    }
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
    // This is a compare-and-swap claim. A delayed page can only replace the epoch it actually
    // observed; once another page wins, the delayed mutation is rejected regardless of either
    // client's wall clock.
    if (args.expectedControllerEpoch !== session.controllerEpoch) {
      throw new ConvexError("Controller claim is stale");
    }
    const now = Date.now();
    await ctx.db.patch(args.sessionId, {
      controllerEpoch: args.controllerEpoch,
      controllerStartedAt: now,
      hostEpoch: undefined,
      hostStartedAt: undefined,
      hostGeneration: undefined,
      updatedAt: now,
    });
    return { controllerEpoch: args.controllerEpoch };
  },
});

export const end = mutation({
  args: {
    sessionId: v.id("sessions"),
    reason: v.string(),
    controllerEpoch: v.optional(v.string()),
  },
  handler: async (ctx, args) => {
    const identity = await requireIdentity(ctx);
    const session = await ctx.db.get(args.sessionId);
    if (!session || session.ownerId !== identity.subject)
      throw new ConvexError("Session not found");
    if (
      args.controllerEpoch !== undefined &&
      (args.controllerEpoch.length < 1 || args.controllerEpoch.length > 128)
    ) {
      throw new ConvexError("Invalid controller epoch");
    }
    // Automatic browser failure cleanup is scoped to the peer that observed the failure. A
    // delayed mutation from an old page must not end a replacement controller's session.
    if (args.controllerEpoch !== undefined && session.controllerEpoch !== args.controllerEpoch) {
      return null;
    }
    if (session.state === "ended" || session.state === "failed") return null;
    const now = Date.now();
    await ctx.db.patch(args.sessionId, {
      state: "ended",
      endedAt: now,
      updatedAt: now,
      endReason: args.reason.slice(0, 256),
    });
    await ctx.db.insert("auditEvents", {
      ownerId: session.ownerId,
      deviceId: session.deviceId,
      sessionId: args.sessionId,
      action: "session.ended",
      detail: args.reason.slice(0, 256),
      createdAt: now,
    });
    return null;
  },
});

export const authorizeTurn = internalQuery({
  args: { sessionId: v.id("sessions"), ownerId: v.string() },
  handler: async (ctx, args) => {
    const session = await ctx.db.get(args.sessionId);
    return Boolean(
      session &&
      session.ownerId === args.ownerId &&
      session.expiresAt > Date.now() &&
      session.state !== "ended" &&
      session.state !== "failed",
    );
  },
});

export const turnCredentials = action({
  args: { sessionId: v.id("sessions") },
  handler: async (ctx, args) => {
    const identity = await ctx.auth.getUserIdentity();
    if (!identity) throw new ConvexError("Unauthenticated");
    const authorized = await ctx.runQuery(internal.sessions.authorizeTurn, {
      sessionId: args.sessionId,
      ownerId: identity.subject,
    });
    if (!authorized) throw new ConvexError("Session not found");
    return mintTurnCredentials(String(args.sessionId));
  },
});
