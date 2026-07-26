import { type NextRequest, NextResponse } from "next/server";

function convexSources(convexUrl: string | undefined): string[] {
  if (!convexUrl) return [];
  try {
    const url = new URL(convexUrl);
    if (url.protocol !== "https:" && url.protocol !== "http:") return [];
    const websocket = new URL(url);
    websocket.protocol = url.protocol === "https:" ? "wss:" : "ws:";
    return [url.origin, websocket.origin];
  } catch {
    return [];
  }
}

export function buildContentSecurityPolicy(
  nonce: string,
  convexUrl: string | undefined,
  development: boolean,
): string {
  const connectSources = ["'self'", "https://shoo.dev", ...convexSources(convexUrl)];
  const directives = [
    "default-src 'self'",
    `script-src 'self' 'nonce-${nonce}' 'strict-dynamic'${development ? " 'unsafe-eval'" : ""}`,
    `style-src 'self' 'nonce-${nonce}'${development ? " 'unsafe-inline'" : ""}`,
    `connect-src ${connectSources.join(" ")}`,
    "img-src 'self' blob: data:",
    "font-src 'self'",
    "media-src 'self' blob:",
    "object-src 'none'",
    "base-uri 'self'",
    "form-action 'self' https://shoo.dev",
    "frame-ancestors 'none'",
    "worker-src 'none'",
  ];
  if (!development) directives.push("upgrade-insecure-requests");
  return `${directives.join("; ")};`;
}

export function middleware(request: NextRequest) {
  const nonce = Buffer.from(crypto.randomUUID()).toString("base64");
  const policy = buildContentSecurityPolicy(
    nonce,
    process.env.NEXT_PUBLIC_CONVEX_URL,
    process.env.NODE_ENV === "development",
  );
  const requestHeaders = new Headers(request.headers);
  requestHeaders.set("x-nonce", nonce);
  requestHeaders.set("Content-Security-Policy", policy);

  const response = NextResponse.next({ request: { headers: requestHeaders } });
  response.headers.set("Content-Security-Policy", policy);
  if (process.env.NODE_ENV === "production") {
    response.headers.set(
      "Strict-Transport-Security",
      "max-age=63072000; includeSubDomains; preload",
    );
  }
  return response;
}

export const config = {
  matcher: [
    {
      source: "/((?!_next/static|_next/image|favicon.ico).*)",
      missing: [
        { type: "header", key: "next-router-prefetch" },
        { type: "header", key: "purpose", value: "prefetch" },
      ],
    },
  ],
};
