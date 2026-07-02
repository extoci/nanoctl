import { randomBytes } from "node:crypto";
import type { SessionStore } from "./types";

function parseCookies(rawCookie: string): Map<string, string> {
  const cookieMap = new Map<string, string>();
  if (!rawCookie) return cookieMap;

  rawCookie.split(";").forEach((chunk) => {
    const [rawName, ...rest] = chunk.trim().split("=");
    if (!rawName || rest.length === 0) return;
    cookieMap.set(rawName, decodeURIComponent(rest.join("=")));
  });

  return cookieMap;
}

export function createSessionManager(ttlMs: number) {
  const sessions: SessionStore = new Map();

  const createSession = (): { id: string; expiresAt: number } => {
    const id = randomBytes(24).toString("base64url");
    const expiresAt = Date.now() + ttlMs;
    sessions.set(id, expiresAt);
    return { id, expiresAt };
  };

  const getValidSessionId = (req: Request): string | null => {
    const cookie = req.headers.get("cookie") ?? "";
    const sessionId = parseCookies(cookie).get("nanoctl_session");
    if (!sessionId) return null;

    const expiresAt = sessions.get(sessionId);
    if (!expiresAt) return null;
    if (Date.now() > expiresAt) {
      sessions.delete(sessionId);
      return null;
    }

    return sessionId;
  };

  return {
    createSession,
    getValidSessionId,
    isAuthorized(req: Request): boolean {
      return getValidSessionId(req) !== null;
    },
    cleanupExpiredSessions(): void {
      const now = Date.now();
      for (const [id, expiresAt] of sessions.entries()) {
        if (expiresAt <= now) sessions.delete(id);
      }
    },
  };
}
