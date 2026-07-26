import { describe, expect, test } from "bun:test";

import { nextRateLimitState } from "./rateLimits";

describe("fixed-window rate limiting", () => {
  test("admits through the limit and denies excess requests", () => {
    const first = nextRateLimitState(null, 1_000, 2, 500);
    expect(first).toEqual({
      windowStartedAt: 1_000,
      count: 1,
      expiresAt: 2_000,
    });
    const second = nextRateLimitState(first, 1_100, 2, 500);
    expect(second?.count).toBe(2);
    expect(nextRateLimitState(second, 1_200, 2, 500)).toBeNull();
  });

  test("starts a new window at the boundary", () => {
    const previous = {
      windowStartedAt: 1_000,
      count: 20,
      expiresAt: 2_000,
    };
    expect(nextRateLimitState(previous, 1_500, 2, 500)).toEqual({
      windowStartedAt: 1_500,
      count: 1,
      expiresAt: 2_500,
    });
  });
});
