"use client";

import { Authenticated, AuthLoading, Unauthenticated } from "convex/react";
import type { ReactNode } from "react";
import { signIn } from "../lib/shoo";

export function AuthGate({ children }: { children: ReactNode }) {
  return (
    <>
      <AuthLoading>
        <main className="centered">
          <p className="eyebrow">nanoctl</p>
          <h1>Opening your workspace…</h1>
        </main>
      </AuthLoading>
      <Unauthenticated>
        <main className="centered">
          <p className="eyebrow">nanoctl / secure remote access</p>
          <h1>Your computers, one quiet click away.</h1>
          <p className="lede">
            Direct WebRTC video and control, with encrypted relay fallback when networks get
            difficult.
          </p>
          <button className="primary" type="button" onClick={() => void signIn()}>
            Continue with Shoo
          </button>
        </main>
      </Unauthenticated>
      <Authenticated>{children}</Authenticated>
    </>
  );
}
