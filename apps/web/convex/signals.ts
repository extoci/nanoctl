import { ConvexError, v } from "convex/values";
import { mutation, query } from "./_generated/server";
import { parseSignalEnvelope, requireIdentity } from "./lib";
import { consumeRateLimit } from "./rateLimits";

const MAX_ENVELOPE_BYTES = 1_100_000;

export const list = query({
  args: { sessionId: v.id("sessions"), afterSequence: v.number() },
  handler: async (ctx, args) => {
    // Host sequence seeds intentionally change after an agent restart. Replay the bounded recent
    // window by database insertion time so a fresh host epoch cannot be hidden merely because its
    // random sequence is lower than the old one. Keep the cursor argument for v1 callers.
    void args.afterSequence;
    const identity = await requireIdentity(ctx);
    const session = await ctx.db.get(args.sessionId);
    if (!session || session.ownerId !== identity.subject)
      throw new ConvexError("Session not found");
    const offerCandidates = await ctx.db
      .query("signals")
      .withIndex("by_session_sender_kind_created", (q) =>
        q.eq("sessionId", args.sessionId).eq("sender", "controller").eq("kind", "offer"),
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
    if (!currentOffer) return [];
    let parsedOffer: ReturnType<typeof parseSignalEnvelope>;
    try {
      parsedOffer = parseSignalEnvelope(currentOffer.envelope, "controller");
    } catch {
      return [];
    }
    if (parsedOffer.controllerEpoch.length === 0) return [];
    const hostRows =
      session.hostEpoch === undefined
        ? []
        : await ctx.db
            .query("signals")
            .withIndex("by_session_sender_epoch_generation_created", (q) =>
              q
                .eq("sessionId", args.sessionId)
                .eq("sender", "host")
                .eq("controllerEpoch", parsedOffer.controllerEpoch)
                .eq("peerEpoch", session.hostEpoch)
                .eq("generation", parsedOffer.generation),
            )
            .order("desc")
            .take(512);
    const latestAnswerCandidate =
      session.hostEpoch === undefined
        ? undefined
        : await ctx.db
            .query("signals")
            .withIndex("by_session_sender_kind_epoch_generation_created", (q) =>
              q
                .eq("sessionId", args.sessionId)
                .eq("sender", "host")
                .eq("kind", "answer")
                .eq("controllerEpoch", parsedOffer.controllerEpoch)
                .eq("peerEpoch", session.hostEpoch)
                .eq("generation", parsedOffer.generation),
            )
            .order("desc")
            .first();
    const latestAnswer =
      latestAnswerCandidate && latestAnswerCandidate._creationTime >= currentOffer._creationTime
        ? latestAnswerCandidate
        : undefined;
    // A host restart can reuse a low/random sequence and an old peer can publish after the new
    // offer. Filter by the controller epoch and insertion boundary, not by sequence or wall-clock
    // milliseconds. Then keep only the latest answer epoch and everything published after it;
    // otherwise a reactive retry could feed two answers into one RTCPeerConnection.
    const relevant = hostRows
      .filter((row) => row._creationTime >= currentOffer._creationTime)
      .filter((row) => {
        try {
          const parsed = parseSignalEnvelope(row.envelope, "host");
          return (
            parsed.controllerEpoch === parsedOffer.controllerEpoch &&
            parsed.generation === parsedOffer.generation &&
            session.hostEpoch !== undefined &&
            parsed.hostEpoch === session.hostEpoch
          );
        } catch {
          return false;
        }
      })
      .sort((left, right) => left._creationTime - right._creationTime);
    // Candidates can be inserted before the answer and still be required for connectivity. Keep
    // every current-generation candidate/completion row, while retaining only the newest answer so
    // a reactive replay cannot feed two SDP answers into one RTCPeerConnection.
    return relevant.filter((row) => row.kind !== "answer" || row._id === latestAnswer?._id);
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
    if (parsed.version !== 2) throw new ConvexError("Unsupported signal protocol");
    if (parsed.sessionId !== String(args.sessionId)) {
      throw new ConvexError("Invalid signal envelope");
    }
    if (
      parsed.controllerEpoch.length === 0 ||
      (session.controllerEpoch !== undefined &&
        parsed.controllerEpoch !== session.controllerEpoch) ||
      (session.controllerEpoch === undefined && parsed.kind !== "offer")
    ) {
      throw new ConvexError("Controller peer is no longer current");
    }
    const duplicate = await ctx.db
      .query("signals")
      .withIndex("by_session_sender_epoch_sequence", (q) =>
        q
          .eq("sessionId", args.sessionId)
          .eq("sender", "controller")
          .eq("peerEpoch", parsed.controllerEpoch)
          .eq("sequence", parsed.sequence),
      )
      .unique();
    if (duplicate) return null;
    const offerCandidates = await ctx.db
      .query("signals")
      .withIndex("by_session_sender_kind_created", (q) =>
        q.eq("sessionId", args.sessionId).eq("sender", "controller").eq("kind", "offer"),
      )
      .order("desc")
      .take(64);
    const currentOffer = offerCandidates
      .sort((left, right) => right._creationTime - left._creationTime)
      .find((row) => {
        if (session.controllerEpoch === undefined) {
          if (parsed.kind !== "offer" || parsed.controllerEpoch.length === 0) return false;
          try {
            return (
              parseSignalEnvelope(row.envelope, "controller").controllerEpoch ===
              parsed.controllerEpoch
            );
          } catch {
            return false;
          }
        }
        try {
          return (
            parseSignalEnvelope(row.envelope, "controller").controllerEpoch ===
            parsed.controllerEpoch
          );
        } catch {
          return false;
        }
      });
    if (currentOffer) {
      let currentOfferSignal: ReturnType<typeof parseSignalEnvelope>;
      try {
        currentOfferSignal = parseSignalEnvelope(currentOffer.envelope, "controller");
      } catch {
        throw new ConvexError("Current offer is malformed");
      }
      if (
        parsed.kind === "offer"
          ? parsed.generation <= currentOfferSignal.generation
          : parsed.generation !== currentOfferSignal.generation
      ) {
        throw new ConvexError("Signal belongs to an inactive generation");
      }
    } else if (parsed.kind !== "offer" && parsed.kind !== "end") {
      throw new ConvexError("Signal has no active offer");
    }
    const now = Date.now();
    if (session.controllerEpoch === undefined) {
      await ctx.db.patch(args.sessionId, {
        controllerEpoch: parsed.controllerEpoch,
        controllerStartedAt: now,
        hostEpoch: undefined,
        hostStartedAt: undefined,
        hostGeneration: undefined,
        updatedAt: now,
      });
    }
    await ctx.db.insert("signals", {
      sessionId: args.sessionId,
      sender: "controller",
      kind: parsed.kind,
      sequence: parsed.sequence,
      envelope: args.envelope,
      createdAt: now,
      peerEpoch: parsed.controllerEpoch,
      controllerEpoch: parsed.controllerEpoch,
      generation: parsed.generation,
      expiresAt: session.expiresAt,
    });
    if (parsed.kind === "end") {
      await ctx.db.patch(args.sessionId, {
        state: "ended",
        endedAt: now,
        updatedAt: now,
        endReason: "ended by controller signal",
      });
      await ctx.db.insert("auditEvents", {
        ownerId: session.ownerId,
        deviceId: session.deviceId,
        sessionId: args.sessionId,
        action: "session.ended",
        detail: "ended by controller signal",
        createdAt: now,
      });
    } else if (session.state === "requested") {
      await ctx.db.patch(args.sessionId, { state: "negotiating", updatedAt: now });
    }
    return null;
  },
});
