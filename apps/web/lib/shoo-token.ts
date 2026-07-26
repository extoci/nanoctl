export type ShooTokenDisposition = "use" | "expired" | "reauthenticate";

export function shooTokenDisposition(
  expiresAt: number | null,
  forceRefreshToken: boolean,
  now: number,
): ShooTokenDisposition {
  if (expiresAt !== null && expiresAt <= now) return "expired";
  if (forceRefreshToken && expiresAt !== null && expiresAt <= now + 30_000) {
    return "reauthenticate";
  }
  return "use";
}
