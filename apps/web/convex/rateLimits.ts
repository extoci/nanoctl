import { v } from "convex/values";
import { internalMutation, type MutationCtx } from "./_generated/server";

type RateLimitState = {
  windowStartedAt: number;
  count: number;
  expiresAt: number;
};

export function nextRateLimitState(
  existing: RateLimitState | null,
  now: number,
  limit: number,
  windowMs: number,
): RateLimitState | null {
  if (!existing || existing.windowStartedAt + windowMs <= now) {
    return {
      windowStartedAt: now,
      count: 1,
      expiresAt: now + windowMs * 2,
    };
  }
  if (existing.count >= limit) return null;
  return {
    windowStartedAt: existing.windowStartedAt,
    count: existing.count + 1,
    expiresAt: now + windowMs * 2,
  };
}

export async function consumeRateLimit(
  ctx: MutationCtx,
  key: string,
  limit: number,
  windowMs: number,
): Promise<boolean> {
  if (key.length > 256 || limit < 1 || windowMs < 1) return false;
  const now = Date.now();
  const existing = await ctx.db
    .query("rateLimits")
    .withIndex("by_key", (q) => q.eq("key", key))
    .unique();
  const next = nextRateLimitState(existing, now, limit, windowMs);
  if (!next) return false;
  if (!existing) {
    await ctx.db.insert("rateLimits", { key, ...next });
    return true;
  }
  await ctx.db.patch(existing._id, next);
  return true;
}

export const consume = internalMutation({
  args: {
    key: v.string(),
    limit: v.number(),
    windowMs: v.number(),
  },
  handler: async (ctx, args) => consumeRateLimit(ctx, args.key, args.limit, args.windowMs),
});
