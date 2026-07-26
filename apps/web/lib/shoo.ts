"use client";

import { createShooAuth, useShooAuth } from "@shoojs/react";
import { useCallback } from "react";

const options = { callbackPath: "/auth/callback" } as const;

export function useShooConvexAuth() {
  const { identity, loading } = useShooAuth(options);
  const fetchAccessToken = useCallback(
    async (_options: { forceRefreshToken: boolean }) => identity.token ?? null,
    [identity.token],
  );
  return {
    isLoading: loading,
    isAuthenticated: Boolean(identity.userId && identity.token),
    fetchAccessToken,
  };
}

export async function signIn(): Promise<void> {
  if (typeof window === "undefined") return;
  await createShooAuth(options).startSignIn();
}

export function signOut(): void {
  if (typeof window === "undefined") return;
  createShooAuth(options).clearIdentity();
  window.location.assign("/");
}
