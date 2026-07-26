import { internalMutation } from "./_generated/server";

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
  },
});
