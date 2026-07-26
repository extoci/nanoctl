"use client";

import { createShooAuth, useShooAuth } from "@shoojs/react";
import { useCallback } from "react";
import { shooTokenDisposition } from "./shoo-token";

const options = { callbackPath: "/auth/callback" } as const;
let reauthInFlight: Promise<void> | null = null;

function beginReauthentication(): void {
  if (reauthInFlight || typeof window === "undefined") return;
  reauthInFlight = createShooAuth(options)
    .startSignIn()
    .then(() => undefined)
    .catch(() => undefined)
    .finally(() => {
      reauthInFlight = null;
    });
}

export function useShooConvexAuth() {
  const { identity, claims, loading, clearIdentity } = useShooAuth(options);
  const expiresAt = typeof claims?.exp === "number" ? claims.exp * 1000 : null;
  const expired = expiresAt !== null && expiresAt <= Date.now();
  const fetchAccessToken = useCallback(
    async (request: { forceRefreshToken: boolean }) => {
      if (!identity.token) return null;
      const disposition = shooTokenDisposition(expiresAt, request.forceRefreshToken, Date.now());
      if (disposition === "expired") {
        clearIdentity();
        if (request.forceRefreshToken) beginReauthentication();
        return null;
      }
      if (disposition === "reauthenticate") {
        beginReauthentication();
        return null;
      }
      return identity.token;
    },
    [clearIdentity, expiresAt, identity.token],
  );
  return {
    isLoading: loading,
    isAuthenticated: Boolean(identity.userId && identity.token && !expired),
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
