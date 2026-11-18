import { v } from "convex/values";
import { internalMutation, internalQuery } from "./_generated/server";
import { parseSignalEnvelope } from "./lib";
import { consumeRateLimit } from "./rateLimits";

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
  args: {
    deviceId: v.id("devices"),
    agentVersion: v.string(),
    capabilitiesJson: v.string(),
    heartbeatSeconds: v.optional(v.number()),
  },
  handler: async (ctx, args) => {
    if (args.heartbeatSeconds !== undefined && !Number.isFinite(args.heartbeatSeconds)) {
      throw new Error("Invalid heartbeat interval");
    }
    const device = await ctx.db.get(args.deviceId);
    if (!device || device.status === "disabled") return false;
    await ctx.db.patch(args.deviceId, {
      lastSeenAt: Date.now(),
      heartbeatSeconds:
        args.heartbeatSeconds === undefined
          ? undefined
          : Math.min(300, Math.max(5, args.heartbeatSeconds)),
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
    const sessions = (
      await Promise.all(
        (["negotiating", "connected"] as const).map((state) =>
          ctx.db
            .query("sessions")
            .withIndex("by_device_state", (q) => q.eq("deviceId", args.deviceId).eq("state", state))
            .collect(),
        ),
      )
    ).flat();
    const pending = await Promise.all(
      sessions
        .filter((session) => session.expiresAt > Date.now())
        .map(async (session) => {
          const offers = await ctx.db
            .query("signals")
            .withIndex("by_session_sender_kind_created", (q) =>
              q.eq("sessionId", session._id).eq("sender", "controller").eq("kind", "offer"),
            )
            .order("desc")
            .take(64);
          const offer = offers
            .sort((left, right) => right._creationTime - left._creationTime)
            .find((row) => {
              if (session.controllerEpoch === undefined) return true;
              try {
                return (
                  parseSignalEnvelope(row.envelope, "controller").controllerEpoch ===
                  session.controllerEpoch
                );
              } catch {
                return false;
              }
            });
          if (!offer) return null;
          let parsedOffer: ReturnType<typeof parseSignalEnvelope>;
          try {
            parsedOffer = parseSignalEnvelope(offer.envelope, "controller");
          } catch {
            return null;
          }
          // Protocol-1 rows remain inspectable, but they cannot start a v2 native peer because
          // they have no controller ownership epoch. Do not keep retrying an upgrade-required
          // legacy offer on every agent poll.
          if (parsedOffer.controllerEpoch.length === 0) return null;
          const latest = await ctx.db
            .query("signals")
            .withIndex("by_session_sender_created", (q) =>
              q.eq("sessionId", session._id).eq("sender", "controller"),
            )
            .order("desc")
            .take(512)
            .then((rows) =>
              rows.filter((row) => {
                if (row._creationTime < offer._creationTime) return false;
                try {
                  const parsed = parseSignalEnvelope(row.envelope, "controller");
                  return (
                    parsed.controllerEpoch === parsedOffer.controllerEpoch &&
                    parsed.generation === parsedOffer.generation
                  );
                } catch {
                  return false;
                }
              }),
            );
          const signals = [
            ...new Map([offer, ...latest].filter(Boolean).map((row) => [row!._id, row!])).values(),
          ].sort(
            (left, right) =>
              left._creationTime - right._creationTime || left.sequence - right.sequence,
          );
          return {
            sessionId: session._id,
            expiresAt: session.expiresAt,
            controllerEpoch: session.controllerEpoch,
            hostEpoch: session.hostEpoch,
            signals,
          };
        }),
    );
    return pending.filter((session) => session !== null);
  },
});

export const sendSignal = internalMutation({
  args: {
    deviceId: v.id("devices"),
    sessionId: v.string(),
    sequence: v.number(),
    envelope: v.string(),
  },
  handler: async (ctx, args) => {
    const sessionId = ctx.db.normalizeId("sessions", args.sessionId);
    if (!sessionId) return false;
    if (!(await consumeRateLimit(ctx, `signal:host:${sessionId}`, 512, 60 * 1000))) {
      return false;
    }
    const session = await ctx.db.get(sessionId);
    if (
      !session ||
      session.deviceId !== args.deviceId ||
      session.expiresAt <= Date.now() ||
      session.state === "ended" ||
      session.state === "failed" ||
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
    if (parsed.version !== 2) return false;
    if (parsed.sessionId !== String(sessionId) || parsed.sequence !== args.sequence) {
      return false;
    }
    const offerCandidates = await ctx.db
      .query("signals")
      .withIndex("by_session_sender_kind_created", (q) =>
        q.eq("sessionId", sessionId).eq("sender", "controller").eq("kind", "offer"),
      )
      .order("desc")
      .take(64);
    const currentOffer = offerCandidates
      .sort((left, right) => right._creationTime - left._creationTime)
      .find((row) => {
        if (session.controllerEpoch === undefined) return true;
        try {
          return (
            parseSignalEnvelope(row.envelope, "controller").controllerEpoch ===
            session.controllerEpoch
          );
        } catch {
          return false;
        }
      });
    if (!currentOffer) return false;
    let parsedOffer: ReturnType<typeof parseSignalEnvelope>;
    try {
      parsedOffer = parseSignalEnvelope(currentOffer.envelope, "controller");
    } catch {
      return false;
    }
    // Host rows are accepted only from the currently visible controller peer. This prevents a
    // delayed answer/end from an old native peer from changing the refreshed session's state.
    if (
      parsed.controllerEpoch !== parsedOffer.controllerEpoch ||
      parsed.generation !== parsedOffer.generation
    ) {
      return false;
    }
    if (session.hostEpoch === undefined || parsed.hostEpoch !== session.hostEpoch) {
      return false;
    }
    if (session.hostGeneration !== undefined && parsed.generation !== session.hostGeneration) {
      return false;
    }
    const duplicate = await ctx.db
      .query("signals")
      .withIndex("by_session_sender_epoch_sequence", (q) =>
        q
          .eq("sessionId", sessionId)
          .eq("sender", "host")
          .eq("peerEpoch", parsed.hostEpoch)
          .eq("sequence", args.sequence),
      )
      .unique();
    if (!duplicate) {
      await ctx.db.insert("signals", {
        sessionId,
        sender: "host",
        kind: parsed.kind,
        sequence: args.sequence,
        envelope: args.envelope,
        createdAt: Date.now(),
        peerEpoch: parsed.hostEpoch,
        controllerEpoch: parsed.controllerEpoch,
        generation: parsed.generation,
        expiresAt: session.expiresAt,
      });
      if (parsed.kind === "answer" && session.state === "negotiating") {
        const now = Date.now();
        await ctx.db.patch(sessionId, { state: "connected", updatedAt: now });
        await ctx.db.insert("auditEvents", {
          ownerId: session.ownerId,
          deviceId: session.deviceId,
          sessionId,
          action: "session.connected",
          createdAt: now,
        });
      } else if (parsed.kind === "end") {
        const now = Date.now();
        const reason = parsed.reason?.slice(0, 256) ?? "agent media pipeline failed";
        await ctx.db.patch(sessionId, {
          state: "failed",
          endedAt: now,
          updatedAt: now,
          endReason: reason,
        });
        await ctx.db.insert("auditEvents", {
          ownerId: session.ownerId,
          deviceId: session.deviceId,
          sessionId,
          action: "session.failed",
          detail: reason,
          createdAt: now,
        });
      }
    }
    return true;
  },
});

/** Claim one native peer incarnation before it publishes an answer or terminal status. */
export const claimHost = internalMutation({
  args: {
    deviceId: v.id("devices"),
    sessionId: v.string(),
    controllerEpoch: v.string(),
    hostEpoch: v.string(),
    expectedHostEpoch: v.optional(v.string()),
    generation: v.number(),
  },
  handler: async (ctx, args) => {
    if (
      args.controllerEpoch.length < 1 ||
      args.controllerEpoch.length > 128 ||
      args.hostEpoch.length < 1 ||
      args.hostEpoch.length > 128 ||
      (args.expectedHostEpoch !== undefined &&
        (args.expectedHostEpoch.length < 1 || args.expectedHostEpoch.length > 128)) ||
      !Number.isSafeInteger(args.generation) ||
      args.generation < 0 ||
      args.generation > 1_000_000
    ) {
      return false;
    }
    const sessionId = ctx.db.normalizeId("sessions", args.sessionId);
    if (!sessionId) return false;
    const session = await ctx.db.get(sessionId);
    if (
      !session ||
      session.deviceId !== args.deviceId ||
      session.expiresAt <= Date.now() ||
      session.state === "ended" ||
      session.state === "failed" ||
      session.controllerEpoch !== args.controllerEpoch
    ) {
      return false;
    }
    // Compare-and-swap the host epoch. A second process cannot replace a peer based on a
    // skewed/native wall clock after another claim has already won.
    if (args.expectedHostEpoch !== session.hostEpoch) {
      return false;
    }
    // A native process restart may happen after the old peer has already answered. The new host
    // epoch is still allowed to claim the controller/generation: the browser treats the epoch
    // change as a recovery boundary, ignores the replacement's bootstrap answer, and sends a new
    // ICE-restart offer before accepting media. The compare-and-swap above ensures only one host
    // process can own that replacement epoch.
    const offers = await ctx.db
      .query("signals")
      .withIndex("by_session_sender_kind_created", (q) =>
        q.eq("sessionId", sessionId).eq("sender", "controller").eq("kind", "offer"),
      )
      .order("desc")
      .take(64);
    const currentOffer = offers
      .sort((left, right) => right._creationTime - left._creationTime)
      .find((row) => {
        try {
          return (
            parseSignalEnvelope(row.envelope, "controller").controllerEpoch === args.controllerEpoch
          );
        } catch {
          return false;
        }
      });
    if (!currentOffer) return false;
    let parsedOffer: ReturnType<typeof parseSignalEnvelope>;
    try {
      parsedOffer = parseSignalEnvelope(currentOffer.envelope, "controller");
    } catch {
      return false;
    }
    if (parsedOffer.generation !== args.generation) return false;
    const now = Date.now();
    await ctx.db.patch(sessionId, {
      hostEpoch: args.hostEpoch,
      hostStartedAt: now,
      hostGeneration: args.generation,
      updatedAt: now,
    });
    return true;
  },
});

export const authorizeSession = internalQuery({
  args: { deviceId: v.id("devices"), sessionId: v.string() },
  handler: async (ctx, args) => {
    const sessionId = ctx.db.normalizeId("sessions", args.sessionId);
    if (!sessionId) return false;
    const session = await ctx.db.get(sessionId);
    return Boolean(
      session &&
      session.deviceId === args.deviceId &&
      session.expiresAt > Date.now() &&
      session.state !== "ended" &&
      session.state !== "failed",
    );
  },
});
