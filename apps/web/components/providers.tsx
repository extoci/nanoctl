"use client";

import { ConvexProviderWithAuth, ConvexReactClient } from "convex/react";
import type { ReactNode } from "react";
import { useShooConvexAuth } from "../lib/shoo";

const convexUrl = process.env.NEXT_PUBLIC_CONVEX_URL;
const convex = convexUrl ? new ConvexReactClient(convexUrl) : null;

export function Providers({ children }: { children: ReactNode }) {
  if (!convex) {
    return <div className="configuration-error">NEXT_PUBLIC_CONVEX_URL is not configured.</div>;
  }
  return (
    <ConvexProviderWithAuth client={convex} useAuth={useShooConvexAuth}>
      {children}
    </ConvexProviderWithAuth>
  );
}
