import type { NextConfig } from "next";

function controlPlaneOrigin(): string {
  const configured = process.env.NANOCTL_CONTROL_PLANE_ORIGIN;
  if (configured) return new URL(configured).origin;

  const deployment = new URL(process.env.NEXT_PUBLIC_CONVEX_URL ?? "http://127.0.0.1:3211");
  if (deployment.hostname.endsWith(".convex.cloud")) {
    deployment.hostname = deployment.hostname.replace(/\.convex\.cloud$/, ".convex.site");
  }
  return deployment.origin;
}

const nextConfig: NextConfig = {
  poweredByHeader: false,
  reactStrictMode: true,
  typedRoutes: true,
  headers: async () => [
    {
      source: "/:path*",
      headers: [
        { key: "Referrer-Policy", value: "strict-origin-when-cross-origin" },
        { key: "X-Content-Type-Options", value: "nosniff" },
        { key: "X-Frame-Options", value: "DENY" },
        {
          key: "Permissions-Policy",
          value: "camera=(), microphone=(), geolocation=(), payment=(), usb=()",
        },
      ],
    },
  ],
  rewrites: async () => [
    {
      source: "/v1/agent/:path*",
      destination: `${controlPlaneOrigin()}/v1/agent/:path*`,
    },
  ],
};

export default nextConfig;
