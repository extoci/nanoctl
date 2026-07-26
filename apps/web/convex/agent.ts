import { v } from "convex/values";
import { internalMutation, internalQuery } from "./_generated/server";
import { parseSignalEnvelope } from "./lib";

export const enroll = internalMutation({
  args: {
    codeHash: v.string(),
    tokenHash: v.string(),
    name: v.string(),
    platform: v.union(v.literal("windows"), v.literal("macos"), v.literal("linux")),
    architecture: v.union(v.literal("x64"), v.literal("arm64")),
    agentVersion: v.string(),
    capabilitiesJson: v.string(),
  },
  handler: async (ctx, args) => {
    const pairing = await ctx.db
      .query("pairingCodes")
      .withIndex("by_code_hash", (q) => q.eq("codeHash", args.codeHash))
      .unique();
    const now = Date.now();
    if (!pairing || pairing.consumedAt || pairing.expiresAt <= now) return null;
    await ctx.db.patch(pairing._id, { consumedAt: now });
    const deviceId = await ctx.db.insert("devices", {
      ownerId: pairing.ownerId,
      name: args.name.slice(0, 80),
      platform: args.platform,
      architecture: args.architecture,
      agentVersion: args.agentVersion.slice(0, 32),
      status: "online",
      tokenHash: args.tokenHash,
      capabilitiesJson: args.capabilitiesJson.slice(0, 64_000),
      lastSeenAt: now,
      createdAt: now,
    });
    await ctx.db.insert("auditEvents", {
      ownerId: pairing.ownerId,
      deviceId,
      action: "device.enrolled",
      createdAt: now,
    });
    return { deviceId, ownerId: pairing.ownerId };
  },
});

export const authenticate = internalQuery({
  args: { tokenHash: v.string() },
  handler: async (ctx, args) => {
    const device = await ctx.db
      .query("devices")
      .withIndex("by_token_hash", (q) => q.eq("tokenHash", args.tokenHash))
      .unique();
    if (!device || device.status === "disabled") return null;
    return { deviceId: device._id, ownerId: device.ownerId };
  },
});

export const heartbeat = internalMutation({
  args: { deviceId: v.id("devices"), agentVersion: v.string(), capabilitiesJson: v.string() },
  handler: async (ctx, args) => {
    const device = await ctx.db.get(args.deviceId);
    if (!device || device.status === "disabled") return false;
    await ctx.db.patch(args.deviceId, {
      lastSeenAt: Date.now(),
      status: "online",
      agentVersion: args.agentVersion.slice(0, 32),
      capabilitiesJson: args.capabilitiesJson.slice(0, 64_000),
    });
    return true;
  },
});

export const pendingSessions = internalQuery({
  args: { deviceId: v.id("devices") },
  handler: async (ctx, args) => {
    const sessions = await ctx.db
      .query("sessions")
      .withIndex("by_device_state", (q) =>
        q.eq("deviceId", args.deviceId).eq("state", "negotiating"),
      )
      .collect();
    return Promise.all(
      sessions
        .filter((session) => session.expiresAt > Date.now())
        .map(async (session) => {
          const latest = await ctx.db
            .query("signals")
            .withIndex("by_session_sender_sequence", (q) =>
              q.eq("sessionId", session._id).eq("sender", "controller"),
            )
            .order("desc")
            .take(127);
          const offer = await ctx.db
            .query("signals")
            .withIndex("by_session_sender_kind_sequence", (q) =>
              q.eq("sessionId", session._id).eq("sender", "controller").eq("kind", "offer"),
            )
            .first();
          const signals = [
            ...new Map([offer, ...latest].filter(Boolean).map((row) => [row!._id, row!])).values(),
          ].sort((left, right) => left.sequence - right.sequence);
          return { sessionId: session._id, expiresAt: session.expiresAt, signals };
        }),
    );
  },
});

export const sendSignal = internalMutation({
  args: {
    deviceId: v.id("devices"),
    sessionId: v.id("sessions"),
    sequence: v.number(),
    envelope: v.string(),
  },
  handler: async (ctx, args) => {
    const session = await ctx.db.get(args.sessionId);
    if (
      !session ||
      session.deviceId !== args.deviceId ||
      session.expiresAt <= Date.now() ||
      session.state === "ended" ||
      args.envelope.length > 1_100_000
    ) {
      return false;
    }
    let parsed: ReturnType<typeof parseSignalEnvelope>;
    try {
      parsed = parseSignalEnvelope(args.envelope, "host");
    } catch {
      return false;
    }
    if (parsed.sessionId !== String(args.sessionId) || parsed.sequence !== args.sequence) {
      return false;
    }
    const duplicate = await ctx.db
      .query("signals")
      .withIndex("by_session_sender_sequence", (q) =>
        q.eq("sessionId", args.sessionId).eq("sender", "host").eq("sequence", args.sequence),
      )
      .unique();
    if (!duplicate) {
      await ctx.db.insert("signals", {
        sessionId: args.sessionId,
        sender: "host",
        kind: parsed.kind,
        sequence: args.sequence,
        envelope: args.envelope,
        createdAt: Date.now(),
        expiresAt: session.expiresAt,
      });
    }
    return true;
  },
});

export const authorizeSession = internalQuery({
  args: { deviceId: v.id("devices"), sessionId: v.id("sessions") },
  handler: async (ctx, args) => {
    const session = await ctx.db.get(args.sessionId);
    return Boolean(
      session &&
      session.deviceId === args.deviceId &&
      session.expiresAt > Date.now() &&
      session.state !== "ended" &&
      session.state !== "failed",
    );
  },
});
