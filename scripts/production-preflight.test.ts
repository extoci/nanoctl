import { describe, expect, test } from "bun:test";

import { validateProductionEnvironment } from "./production-preflight";

describe("production deployment preflight", () => {
  test("accepts canonical HTTPS origins", () => {
    expect(() =>
      validateProductionEnvironment({
        NEXT_PUBLIC_CONVEX_URL: "https://nanoctl.convex.cloud",
        NEXT_PUBLIC_APP_ORIGIN: "https://remote.nanoctl.dev",
      }),
    ).not.toThrow();
  });

  test("rejects local, non-Convex, and non-origin values", () => {
    expect(() =>
      validateProductionEnvironment({
        NEXT_PUBLIC_CONVEX_URL: "http://127.0.0.1:3210",
        NEXT_PUBLIC_APP_ORIGIN: "https://remote.nanoctl.dev",
      }),
    ).toThrow("credential-free HTTPS origin");
    expect(() =>
      validateProductionEnvironment({
        NEXT_PUBLIC_CONVEX_URL: "https://database.example.com",
        NEXT_PUBLIC_APP_ORIGIN: "https://remote.nanoctl.dev",
      }),
    ).toThrow("Convex cloud deployment");
    expect(() =>
      validateProductionEnvironment({
        NEXT_PUBLIC_CONVEX_URL: "https://nanoctl.convex.cloud/path",
        NEXT_PUBLIC_APP_ORIGIN: "https://remote.nanoctl.dev",
      }),
    ).toThrow("credential-free HTTPS origin");
  });
});
