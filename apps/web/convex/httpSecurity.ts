export async function pseudonymousAddress(
  address: string | null,
  secret: string | undefined,
): Promise<string> {
  if (!secret) return sha256("rate-limit-secret-unconfigured");
  const key = await crypto.subtle.importKey(
    "raw",
    new TextEncoder().encode(secret),
    { name: "HMAC", hash: "SHA-256" },
    false,
    ["sign"],
  );
  const signature = await crypto.subtle.sign(
    "HMAC",
    key,
    new TextEncoder().encode(address ?? "unknown"),
  );
  return hex(new Uint8Array(signature));
}

async function sha256(value: string): Promise<string> {
  return hex(
    new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(value))),
  );
}

function hex(bytes: Uint8Array): string {
  return [...bytes].map((byte) => byte.toString(16).padStart(2, "0")).join("");
}
