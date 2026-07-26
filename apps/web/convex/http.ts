import { httpRouter } from "convex/server";
import { httpAction } from "./_generated/server";
import { internal } from "./_generated/api";

const http = httpRouter();

http.route({
  path: "/v1/agent/enroll",
  method: "POST",
  handler: httpAction(async (ctx, request) => {
    const parsed = await readJson(request, 70_000);
    if (!parsed.ok) return parsed.response;
    const body = parsed.value;
    if (
      typeof body.code !== "string" ||
      typeof body.name !== "string" ||
      !isPlatform(body.platform) ||
      !isArchitecture(body.architecture) ||
      typeof body.agentVersion !== "string" ||
      !isRecord(body.capabilities)
    ) {
      return json({ error: "invalid_request" }, 400);
    }
    const token = base64Url(crypto.getRandomValues(new Uint8Array(32)));
    const result = await ctx.runMutation(internal.agent.enroll, {
      codeHash: await sha256(body.code.trim().toUpperCase()),
      tokenHash: await sha256(token),
      name: body.name,
      platform: body.platform,
      architecture: body.architecture,
      agentVersion: body.agentVersion,
      capabilitiesJson: JSON.stringify(body.capabilities),
    });
    if (!result) return json({ error: "invalid_or_expired_code" }, 401);
    return json({ deviceId: result.deviceId, token });
  }),
});

http.route({
  path: "/v1/agent/heartbeat",
  method: "POST",
  handler: httpAction(async (ctx, request) => {
    const auth = await authenticateAgent(ctx, request);
    if (!auth) return json({ error: "unauthorized" }, 401);
    const parsed = await readJson(request, 70_000);
    if (!parsed.ok) return parsed.response;
    const body = parsed.value;
    const ok = await ctx.runMutation(internal.agent.heartbeat, {
      deviceId: auth.deviceId,
      agentVersion: typeof body.agentVersion === "string" ? body.agentVersion : "unknown",
      capabilitiesJson: JSON.stringify(body.capabilities ?? {}),
    });
    return ok ? json({ ok: true }) : json({ error: "disabled" }, 403);
  }),
});

http.route({
  path: "/v1/agent/sessions",
  method: "GET",
  handler: httpAction(async (ctx, request) => {
    const auth = await authenticateAgent(ctx, request);
    if (!auth) return json({ error: "unauthorized" }, 401);
    const sessions = await ctx.runQuery(internal.agent.pendingSessions, {
      deviceId: auth.deviceId,
    });
    return json({ sessions });
  }),
});

http.route({
  path: "/v1/agent/signal",
  method: "POST",
  handler: httpAction(async (ctx, request) => {
    const auth = await authenticateAgent(ctx, request);
    if (!auth) return json({ error: "unauthorized" }, 401);
    const parsed = await readJson(request, 1_200_000);
    if (!parsed.ok) return parsed.response;
    const body = parsed.value;
    if (
      typeof body.sessionId !== "string" ||
      !Number.isSafeInteger(body.sequence) ||
      typeof body.envelope !== "string"
    ) {
      return json({ error: "invalid_request" }, 400);
    }
    const ok = await ctx.runMutation(internal.agent.sendSignal, {
      deviceId: auth.deviceId,
      sessionId: body.sessionId as never,
      sequence: Number(body.sequence),
      envelope: body.envelope,
    });
    return ok ? json({ ok: true }) : json({ error: "session_unavailable" }, 409);
  }),
});

http.route({
  path: "/v1/agent/turn",
  method: "GET",
  handler: httpAction(async (ctx, request) => {
    const auth = await authenticateAgent(ctx, request);
    if (!auth) return json({ error: "unauthorized" }, 401);
    const sessionId = new URL(request.url).searchParams.get("sessionId");
    if (!sessionId) return json({ error: "invalid_request" }, 400);
    const authorized = await ctx.runQuery(internal.agent.authorizeSession, {
      deviceId: auth.deviceId,
      sessionId: sessionId as never,
    });
    if (!authorized) return json({ error: "session_unavailable" }, 404);
    const credentials = await mintTurnCredentials(sessionId);
    return credentials ? json(credentials) : new Response(null, { status: 204 });
  }),
});

async function authenticateAgent(
  ctx: Parameters<Parameters<typeof httpAction>[0]>[0],
  request: Request,
) {
  const header = request.headers.get("authorization") ?? "";
  if (!header.startsWith("Bearer ") || header.length > 512) return null;
  return ctx.runQuery(internal.agent.authenticate, {
    tokenHash: await sha256(header.slice(7)),
  });
}

type JsonRead = { ok: true; value: Record<string, unknown> } | { ok: false; response: Response };

async function readJson(request: Request, maxBytes: number): Promise<JsonRead> {
  const length = Number(request.headers.get("content-length") ?? "0");
  if (Number.isFinite(length) && length > maxBytes) {
    return { ok: false, response: json({ error: "request_too_large" }, 413) };
  }
  const text = await request.text();
  if (new TextEncoder().encode(text).byteLength > maxBytes) {
    return { ok: false, response: json({ error: "request_too_large" }, 413) };
  }
  let value: unknown;
  try {
    value = JSON.parse(text);
  } catch {
    return { ok: false, response: json({ error: "invalid_json" }, 400) };
  }
  if (!isRecord(value)) {
    return { ok: false, response: json({ error: "invalid_json" }, 400) };
  }
  return { ok: true, value };
}

function json(body: unknown, status = 200): Response {
  return Response.json(body, {
    status,
    headers: {
      "cache-control": "no-store",
      "content-type": "application/json; charset=utf-8",
    },
  });
}

function isPlatform(value: unknown): value is "windows" | "macos" | "linux" {
  return value === "windows" || value === "macos" || value === "linux";
}

function isArchitecture(value: unknown): value is "x64" | "arm64" {
  return value === "x64" || value === "arm64";
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}

async function sha256(value: string): Promise<string> {
  const bytes = new TextEncoder().encode(value);
  return [...new Uint8Array(await crypto.subtle.digest("SHA-256", bytes))]
    .map((byte) => byte.toString(16).padStart(2, "0"))
    .join("");
}

function base64Url(bytes: Uint8Array): string {
  return btoa(String.fromCharCode(...bytes))
    .replaceAll("+", "-")
    .replaceAll("/", "_")
    .replaceAll("=", "");
}

async function mintTurnCredentials(sessionId: string) {
  const secret = process.env.TURN_AUTH_SECRET;
  const urls = (process.env.TURN_URLS ?? "")
    .split(",")
    .map((value) => value.trim())
    .filter(Boolean);
  if (!secret || urls.length === 0) return null;
  const expiresAtSeconds = Math.floor(Date.now() / 1000) + 20 * 60;
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

export default http;
