import { describe, expect, test } from "bun:test";
import { pseudonymousAddress } from "./httpSecurity";

describe("HTTP requester pseudonyms", () => {
  test("are stable per secret without exposing the address", async () => {
    const first = await pseudonymousAddress("203.0.113.10", "secret-a");
    expect(first).toHaveLength(64);
    expect(first).not.toContain("203.0.113.10");
    expect(await pseudonymousAddress("203.0.113.10", "secret-a")).toBe(first);
    expect(await pseudonymousAddress("203.0.113.10", "secret-b")).not.toBe(first);
    expect(await pseudonymousAddress("203.0.113.11", "secret-a")).not.toBe(first);
  });

  test("collapses to a non-address fallback when the secret is absent", async () => {
    expect(await pseudonymousAddress("203.0.113.10", undefined)).toBe(
      await pseudonymousAddress("198.51.100.20", undefined),
    );
  });
});
