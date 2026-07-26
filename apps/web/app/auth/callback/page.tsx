"use client";

import { useShooAuth } from "@shoojs/react";

export default function ShooCallbackPage() {
  useShooAuth();
  return (
    <main className="centered">
      <p className="eyebrow">nanoctl / authentication</p>
      <h1>Completing sign-in…</h1>
    </main>
  );
}
