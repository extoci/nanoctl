import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    environment: "edge-runtime",
    include: ["apps/web/convex/**/*.convex.test.ts"],
  },
});
