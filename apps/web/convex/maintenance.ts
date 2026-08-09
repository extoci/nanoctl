import { internalMutation } from "./_generated/server";
import { internal } from "./_generated/api";

const BATCH_SIZE = 256;
const PAIRING_RETENTION_MS = 24 * 60 * 60 * 1000;
const SESSION_RETENTION_MS = 30 * 24 * 60 * 60 * 1000;
const AUDIT_RETENTION_MS = 90 * 24 * 60 * 60 * 1000;

export const purgeExpired = internalMutation({
  args: {},
  handler: async (ctx) => {
    const now = Date.now();
    const signals = await ctx.db
      .query("signals")
      .withIndex("by_expiry", (q) => q.lt("expiresAt", now))
      .take(BATCH_SIZE);
    const expiredActiveSessions = await Promise.all(
      (["requested", "ringing", "negotiating", "connected"] as const).map((state) =>
        ctx.db
          .query("sessions")
          .withIndex("by_state_expiry", (q) => q.eq("state", state).lt("expiresAt", now))
          .take(BATCH_SIZE),
      ),
    );
    const pairingCodes = await ctx.db
      .query("pairingCodes")
      .withIndex("by_expiry", (q) => q.lt("expiresAt", now - PAIRING_RETENTION_MS))
      .take(BATCH_SIZE);
    const endedSessions = await ctx.db
      .query("sessions")
      .withIndex("by_state_updated", (q) =>
        q.eq("state", "ended").lt("updatedAt", now - SESSION_RETENTION_MS),
      )
      .take(BATCH_SIZE);
    const failedSessions = await ctx.db
      .query("sessions")
      .withIndex("by_state_updated", (q) =>
        q.eq("state", "failed").lt("updatedAt", now - SESSION_RETENTION_MS),
      )
      .take(BATCH_SIZE);
    const auditEvents = await ctx.db
      .query("auditEvents")
      .withIndex("by_created", (q) => q.lt("createdAt", now - AUDIT_RETENTION_MS))
      .take(BATCH_SIZE);
    const rateLimits = await ctx.db
      .query("rateLimits")
      .withIndex("by_expiry", (q) => q.lt("expiresAt", now))
      .take(BATCH_SIZE);
    const expiredSessions = expiredActiveSessions.flat();
    await Promise.all(
      expiredSessions.map(async (session) => {
        await ctx.db.patch(session._id, {
          state: "failed",
          endedAt: now,
          updatedAt: now,
          endReason: "session expired",
        });
        await ctx.db.insert("auditEvents", {
          ownerId: session.ownerId,
          deviceId: session.deviceId,
          sessionId: session._id,
          action: "session.failed",
          detail: "session expired",
          createdAt: now,
        });
      }),
    );
    await Promise.all(
      [
        ...signals,
        ...pairingCodes,
        ...endedSessions,
        ...failedSessions,
        ...auditEvents,
        ...rateLimits,
      ].map(async (document) => {
        await ctx.db.delete(document._id);
      }),
    );
    if (
      [
        signals,
        pairingCodes,
        endedSessions,
        failedSessions,
        auditEvents,
        rateLimits,
        ...expiredActiveSessions,
      ].some((batch) => batch.length === BATCH_SIZE)
    ) {
      await ctx.scheduler.runAfter(0, internal.maintenance.purgeExpired);
    }
  },
});
