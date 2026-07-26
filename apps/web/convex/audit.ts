import { query } from "./_generated/server";
import { requireIdentity } from "./lib";

export const listRecent = query({
  args: {},
  handler: async (ctx) => {
    const identity = await requireIdentity(ctx);
    const events = await ctx.db
      .query("auditEvents")
      .withIndex("by_owner_time", (q) => q.eq("ownerId", identity.subject))
      .order("desc")
      .take(50);
    return Promise.all(
      events.map(async (event) => {
        const device = event.deviceId ? await ctx.db.get(event.deviceId) : null;
        return {
          _id: event._id,
          action: event.action,
          detail: event.detail,
          createdAt: event.createdAt,
          deviceName: device?.name,
        };
      }),
    );
  },
});
