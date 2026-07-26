import { describe, expect, test } from "bun:test";
import { shooTokenDisposition } from "./shoo-token";

describe("Shoo token lifecycle", () => {
  test("uses a nearly expired token for ordinary reads", () => {
    expect(shooTokenDisposition(110_000, false, 100_000)).toBe("use");
  });

  test("reauthenticates near expiry only when Convex requests refresh", () => {
    expect(shooTokenDisposition(110_000, true, 100_000)).toBe("reauthenticate");
  });

  test("never returns an expired token", () => {
    expect(shooTokenDisposition(100_000, false, 100_000)).toBe("expired");
  });
});
