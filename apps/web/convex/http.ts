import { httpRouter } from "convex/server";
import { httpAction } from "./_generated/server";
import { internal } from "./_generated/api";

const http = httpRouter();

http.route({
  path: "/v1/agent/enroll",
  method: "POST",
  handler: httpAction(async (ctx, request) => {
    const body = await readJson(request, 70_000);
    if (
      typeof body.code !== "string" ||
      typeof body.name !== "string" ||
      !isPlatform(body.platform) ||
      !isArchitecture(body.architecture) ||
      typeof body.agentVersion !== "string" ||
      typeof body.capabilities !== "object"
    ) {
      return json({ error: "invalid_request" }, 400);
    }
    const token = base64Url(crypto.getRandomValues(new Uint8Array(32)));
    const result = await ctx.runMutation(internal.agent.enroll, {
      codeHash: await sha256(body.code),
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
    const body = await readJson(request, 70_000);
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
    const body = await readJson(request, 1_200_000);
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

async function readJson(request: Request, maxBytes: number): Promise<Record<string, unknown>> {
  const length = Number(request.headers.get("content-length") ?? "0");
  if (length > maxBytes) throw new Error("request_too_large");
  const text = await request.text();
  if (text.length > maxBytes) throw new Error("request_too_large");
  const value: unknown = JSON.parse(text);
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("invalid_json");
  }
  return value as Record<string, unknown>;
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

export default http;
