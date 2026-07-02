#!/usr/bin/env bun

import { randomBytes } from "node:crypto";
import { getRuntimeConfig } from "./config";
import { createControlBridge } from "./control-bridge";
import { createCaptureManager } from "./capture";
import { html, loginPage, watchPage } from "./pages";
import { readPostedOffer, readPostedPin, redirect, unauthorized } from "./request";
import { createSessionManager } from "./session";
import { getLanIp, paint, printCliPanel, setupRuntimeKeyControls } from "./terminal";
import { createViewerManager } from "./webrtc";

const config = getRuntimeConfig();
const SESSION_TTL_MS = 8 * 60 * 60 * 1000;

let teardownKeys = () => {};
let shutdownRef = () => {};
let lastIgnoredSocketWarningAt = 0;

function shouldIgnoreRuntimeError(error: unknown): boolean {
  if (!(error instanceof Error)) return false;
  return error.message.includes("ECONNREFUSED: connection refused, recv");
}

process.on("uncaughtException", (error) => {
  if (shouldIgnoreRuntimeError(error)) {
    const now = Date.now();
    if (now - lastIgnoredSocketWarningAt > 5000) {
      lastIgnoredSocketWarningAt = now;
      console.warn(`Ignoring transient WebRTC socket error: ${error.message}`);
    }
    return;
  }
  throw error;
});

const controlBridge = createControlBridge(config);
const captureManager = createCaptureManager({
  config,
  paint,
  onAudioTopologyChanged: (reason) => viewerManager.closeAllViewers(reason),
  onFatalError: () => {
    shutdownRef();
    process.exit(1);
  },
});
const viewerManager = createViewerManager(
  paint,
  () => captureManager.getState(),
  controlBridge.enabled,
  (event) => controlBridge.send(event),
);
const sessionManager = createSessionManager(SESSION_TTL_MS);

async function main(): Promise<void> {
  await captureManager.start();

  const lanIp = getLanIp();
  const publicUrl = `http://${lanIp}:${config.port}/`;
  const localUrl = `http://127.0.0.1:${config.port}/`;

  setInterval(() => sessionManager.cleanupExpiredSessions(), 60_000).unref();

  const server = Bun.serve({
    hostname: "0.0.0.0",
    port: config.port,
    fetch: async (req: Request) => {
      const url = new URL(req.url);
      const { pathname } = url;

      if (pathname === "/health") return new Response("ok", { status: 200 });
      if (pathname === "/") {
        if (sessionManager.isAuthorized(req)) return redirect("/watch");
        return html(loginPage());
      }

      if (pathname === "/auth" && req.method === "POST") {
        const submitted = await readPostedPin(req);
        if (submitted !== config.pin) return html(loginPage("Incorrect PIN. Try again."));

        const { id, expiresAt } = sessionManager.createSession();
        const expires = new Date(expiresAt).toUTCString();
        return new Response(null, {
          status: 302,
          headers: {
            location: "/watch",
            "set-cookie": `nanoctl_session=${id}; Path=/; HttpOnly; SameSite=Lax; Expires=${expires}`,
          },
        });
      }

      if (pathname === "/watch") {
        if (!sessionManager.isAuthorized(req)) return redirect("/");
        return html(watchPage(controlBridge.enabled));
      }

      if (pathname === "/webrtc/offer" && req.method === "POST") {
        if (!sessionManager.isAuthorized(req)) return unauthorized();
        const offer = await readPostedOffer(req);
        if (!offer) return new Response("Bad offer payload", { status: 400 });

        const viewerId = randomBytes(8).toString("hex");
        try {
          const answer = await viewerManager.createAnswerForOffer(viewerId, offer);
          return new Response(JSON.stringify(answer), {
            status: 200,
            headers: { "content-type": "application/json; charset=utf-8" },
          });
        } catch (error) {
          const message = error instanceof Error ? error.message : String(error);
          console.error(`Offer handling failed for ${viewerId}: ${message}`);
          captureManager.printRecentFfmpegLogs();
          return new Response("Failed to create WebRTC session", { status: 500 });
        }
      }

      return new Response("Not found", { status: 404 });
    },
  });

  let shuttingDown = false;
  const shutdown = () => {
    if (shuttingDown) return;
    shuttingDown = true;
    console.log("\nShutting down...");
    teardownKeys();
    server.stop(true);
    controlBridge.shutdown();
    captureManager.stopEverything(viewerManager.closeAllViewersSync);
    process.exit(0);
  };

  shutdownRef = shutdown;
  teardownKeys = setupRuntimeKeyControls(() => {
    void captureManager.toggleAudioRuntime();
  }, shutdown);

  process.on("SIGINT", shutdown);
  process.on("SIGTERM", shutdown);

  printCliPanel({
    lanUrl: publicUrl,
    localUrl,
    pin: config.pin,
    fps: config.fps,
    videoBitrate: config.videoBitrate,
    audioText: captureManager.getState().audioEnabled
      ? captureManager.getState().audioSourceLabel
      : "Disabled",
    controlText: controlBridge.description,
  });
}

main().catch((error) => {
  console.error(error instanceof Error ? error.message : String(error));
  captureManager.printRecentFfmpegLogs();
  controlBridge.shutdown();
  captureManager.stopEverything(viewerManager.closeAllViewersSync);
  process.exit(1);
});
