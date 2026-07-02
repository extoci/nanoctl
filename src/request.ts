import type { OfferPayload } from "./types";

export async function readPostedPin(req: Request): Promise<string> {
  const contentType = req.headers.get("content-type") ?? "";
  if (!contentType.includes("application/x-www-form-urlencoded")) return "";
  const body = await req.text();
  const params = new URLSearchParams(body);
  return (params.get("pin") ?? "").trim();
}

export async function readPostedOffer(req: Request): Promise<OfferPayload | null> {
  const contentType = req.headers.get("content-type") ?? "";
  if (!contentType.includes("application/json")) return null;

  let parsed: unknown;
  try {
    parsed = await req.json();
  } catch {
    return null;
  }

  if (!parsed || typeof parsed !== "object") return null;

  const maybe = parsed as Record<string, unknown>;
  if (maybe.type !== "offer") return null;
  if (typeof maybe.sdp !== "string" || maybe.sdp.length === 0) return null;

  return {
    type: "offer",
    sdp: maybe.sdp,
  };
}

export function unauthorized(): Response {
  return new Response("Unauthorized", { status: 401 });
}

export function redirect(path: string): Response {
  return new Response(null, {
    status: 302,
    headers: { location: path },
  });
}
