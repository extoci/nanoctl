import type { AuthConfig } from "convex/server";

const appOrigin = process.env.APP_ORIGIN;
if (!appOrigin) {
  throw new Error("APP_ORIGIN is required (for example https://nanoctl.example.com)");
}

export default {
  providers: [
    {
      type: "customJwt",
      issuer: "https://shoo.dev",
      jwks: "https://shoo.dev/.well-known/jwks.json",
      algorithm: "ES256",
      applicationID: `origin:${new URL(appOrigin).origin}`,
    },
  ],
} satisfies AuthConfig;
