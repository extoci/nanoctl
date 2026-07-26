import { ConvexError, v } from "convex/values";
import { action, internalMutation, mutation, query } from "./_generated/server";
import { internal } from "./_generated/api";
import { cleanDeviceName, requireIdentity } from "./lib";

const PAIRING_TTL_MS = 10 * 60 * 1000;
const ONLINE_WINDOW_MS = 45 * 1000;

export const list = query({
  args: {},
  handler: async (ctx) => {
    const identity = await requireIdentity(ctx);
    const devices = await ctx.db
      .query("devices")
      .withIndex("by_owner", (q) => q.eq("ownerId", identity.subject))
      .collect();
    const now = Date.now();
    return devices.map(({ tokenHash: _tokenHash, capabilitiesJson: _capabilities, ...device }) => ({
      ...device,
      status:
        device.status === "disabled"
          ? ("disabled" as const)
          : now - device.lastSeenAt <= ONLINE_WINDOW_MS
            ? ("online" as const)
            : ("offline" as const),
    }));
  },
});

export const createPairingCode = action({
  args: {},
  handler: async (ctx): Promise<{ code: string; expiresAt: number }> => {
    const identity = await ctx.auth.getUserIdentity();
    if (!identity) throw new ConvexError("Unauthenticated");
    const permitted = await ctx.runMutation(internal.rateLimits.consume, {
      key: `pairing:${identity.subject}`,
      limit: 10,
      windowMs: 10 * 60 * 1000,
    });
    if (!permitted) throw new ConvexError("Too many pairing requests");
    const code = pairingCode();
    const codeHash = await sha256(code);
    const expiresAt = Date.now() + PAIRING_TTL_MS;
    await ctx.runMutation(internal.devices.storePairingCode, {
      ownerId: identity.subject,
      codeHash,
      expiresAt,
    });
    return { code, expiresAt };
  },
});

export const storePairingCode = internalMutation({
  args: { ownerId: v.string(), codeHash: v.string(), expiresAt: v.number() },
  handler: async (ctx, args) => {
    await ctx.db.insert("pairingCodes", { ...args, createdAt: Date.now() });
  },
});

export const rename = mutation({
  args: { deviceId: v.id("devices"), name: v.string() },
  handler: async (ctx, args) => {
    const identity = await requireIdentity(ctx);
    const device = await ctx.db.get(args.deviceId);
    if (!device || device.ownerId !== identity.subject) throw new ConvexError("Device not found");
    await ctx.db.patch(args.deviceId, { name: cleanDeviceName(args.name) });
    return null;
  },
});

export const remove = mutation({
  args: { deviceId: v.id("devices") },
  handler: async (ctx, args) => {
    const identity = await requireIdentity(ctx);
    const device = await ctx.db.get(args.deviceId);
    if (!device || device.ownerId !== identity.subject) throw new ConvexError("Device not found");
    const now = Date.now();
    await ctx.db.patch(args.deviceId, {
      status: "disabled",
      disabledAt: now,
      tokenHash: `revoked:${device.tokenHash}`,
    });
    for (const state of ["requested", "ringing", "negotiating", "connected"] as const) {
      const sessions = await ctx.db
        .query("sessions")
        .withIndex("by_device_state", (q) => q.eq("deviceId", args.deviceId).eq("state", state))
        .collect();
      for (const session of sessions) {
        await ctx.db.patch(session._id, {
          state: "ended",
          endedAt: now,
          updatedAt: now,
          endReason: "device revoked",
        });
        await ctx.db.insert("auditEvents", {
          ownerId: identity.subject,
          deviceId: args.deviceId,
          sessionId: session._id,
          action: "session.ended",
          detail: "device revoked",
          createdAt: now,
        });
      }
    }
    await ctx.db.insert("auditEvents", {
      ownerId: identity.subject,
      deviceId: args.deviceId,
      action: "device.revoked",
      createdAt: now,
    });
    return null;
  },
});

async function sha256(value: string): Promise<string> {
  const bytes = new TextEncoder().encode(value);
  return [...new Uint8Array(await crypto.subtle.digest("SHA-256", bytes))]
    .map((byte) => byte.toString(16).padStart(2, "0"))
    .join("");
}

function pairingCode(): string {
  // Twenty Crockford-style base32 characters carry 100 bits of entropy. Ambiguous I/L/O/U
  // characters are omitted so a code can still be entered manually when copy/paste is unavailable.
  const alphabet = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";
  const random = crypto.getRandomValues(new Uint8Array(20));
  const characters = [...random].map((byte) => alphabet[byte & 31]);
  return Array.from({ length: 4 }, (_, index) =>
    characters.slice(index * 5, index * 5 + 5).join(""),
  ).join("-");
}
