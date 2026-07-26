import { internalMutation } from "./_generated/server";

const BATCH_SIZE = 256;
const PAIRING_RETENTION_MS = 24 * 60 * 60 * 1000;
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
      .filter((q) => q.lt(q.field("expiresAt"), now - PAIRING_RETENTION_MS))
      .take(BATCH_SIZE);
    const auditEvents = await ctx.db
      .query("auditEvents")
      .filter((q) => q.lt(q.field("createdAt"), now - AUDIT_RETENTION_MS))
      .take(BATCH_SIZE);
    await Promise.all(
      [...signals, ...pairingCodes, ...auditEvents].map(async (document) => {
        await ctx.db.delete(document._id);
      }),
    );
  },
});
