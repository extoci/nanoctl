import { describe, expect, test } from "bun:test";

import { buildContentSecurityPolicy } from "./middleware";

describe("web content security policy", () => {
  test("permits only the production authentication and control-plane connections", () => {
    const policy = buildContentSecurityPolicy("test-nonce", "https://example.convex.cloud", false);
    expect(policy).toContain(
      "connect-src 'self' https://shoo.dev https://example.convex.cloud wss://example.convex.cloud",
    );
    expect(policy).toContain("script-src 'self' 'nonce-test-nonce' 'strict-dynamic'");
    expect(policy).toContain("frame-ancestors 'none'");
    expect(policy).toContain("upgrade-insecure-requests");
    expect(policy).not.toContain("'unsafe-inline'");
    expect(policy).not.toContain("'unsafe-eval'");
  });

  test("does not reflect unsupported control-plane schemes", () => {
    const policy = buildContentSecurityPolicy("test-nonce", "javascript:alert(1)", false);
    expect(policy).not.toContain("javascript:");
  });

  test("does not upgrade local development control-plane traffic", () => {
    const policy = buildContentSecurityPolicy("test-nonce", "http://127.0.0.1:3210", true);
    expect(policy).toContain("http://127.0.0.1:3210 ws://127.0.0.1:3210");
    expect(policy).not.toContain("upgrade-insecure-requests");
  });
});
