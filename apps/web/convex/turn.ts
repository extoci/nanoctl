const TURN_TTL_SECONDS = 5 * 60;

export function parseTurnUrls(value: string | undefined): string[] {
  if (!value) return [];
  const urls = value
    .split(",")
    .map((entry) => entry.trim())
    .filter(Boolean);
  if (
    urls.length > 8 ||
    urls.some(
      (url) =>
        url.length > 512 ||
        (!url.startsWith("turn:") && !url.startsWith("turns:")) ||
        url.includes("@"),
    )
  ) {
    throw new Error("TURN_URLS contains an invalid relay URL");
  }
  return urls;
}

export async function mintTurnCredentials(sessionId: string) {
  const secret = process.env.TURN_AUTH_SECRET;
  const urls = parseTurnUrls(process.env.TURN_URLS);
  if (!secret && urls.length === 0) return null;
  if (!secret || secret.length < 32 || urls.length === 0) {
    throw new Error("TURN credentials are incompletely configured");
  }
  const expiresAtSeconds = Math.floor(Date.now() / 1000) + TURN_TTL_SECONDS;
  const username = `${expiresAtSeconds}:${sessionId}`;
  const key = await crypto.subtle.importKey(
    "raw",
    new TextEncoder().encode(secret),
    { name: "HMAC", hash: "SHA-1" },
    false,
    ["sign"],
  );
  const signature = await crypto.subtle.sign("HMAC", key, new TextEncoder().encode(username));
  return {
    urls,
    username,
    credential: btoa(String.fromCharCode(...new Uint8Array(signature))),
    expiresAt: expiresAtSeconds * 1000,
  };
}
