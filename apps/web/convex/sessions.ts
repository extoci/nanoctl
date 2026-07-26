import { ConvexError, v } from "convex/values";
import { action, internalQuery, mutation, query } from "./_generated/server";
import { internal } from "./_generated/api";
import { parseDeviceDisplays, requireIdentity } from "./lib";

const SESSION_TTL_MS = 15 * 60 * 1000;

export const getState = query({
  args: { sessionId: v.id("sessions") },
  handler: async (ctx, args) => {
    const identity = await requireIdentity(ctx);
    const session = await ctx.db.get(args.sessionId);
    if (!session || session.ownerId !== identity.subject)
      throw new ConvexError("Session not found");
    const device = await ctx.db.get(session.deviceId);
    return {
      state: session.state,
      expiresAt: session.expiresAt,
      endReason: session.endReason,
      displays: device ? parseDeviceDisplays(device.capabilitiesJson) : [],
    };
  },
});

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
    const secret = process.env.TURN_AUTH_SECRET;
    const urls = (process.env.TURN_URLS ?? "")
      .split(",")
      .map((value) => value.trim())
      .filter(Boolean);
    if (!secret || urls.length === 0) return null;
    const expiresAtSeconds = Math.floor(Date.now() / 1000) + 20 * 60;
    const username = `${expiresAtSeconds}:${args.sessionId}`;
    const key = await crypto.subtle.importKey(
      "raw",
      new TextEncoder().encode(secret),
      { name: "HMAC", hash: "SHA-1" },
      false,
      ["sign"],
    );
    const signature = await crypto.subtle.sign("HMAC", key, new TextEncoder().encode(username));
    return {
      urls,
      username,
      credential: bytesToBase64(new Uint8Array(signature)),
      expiresAt: expiresAtSeconds * 1000,
    };
  },
});

function bytesToBase64(bytes: Uint8Array): string {
  return btoa(String.fromCharCode(...bytes));
}
