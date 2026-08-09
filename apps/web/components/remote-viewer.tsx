"use client";

import { useAction, useConvex, useMutation, useQuery } from "convex/react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { DisplayInfo } from "@nanoctl/protocol";
import { functions } from "../lib/convex";
import {
  RemoteSessionController,
  createControllerEpoch,
  type HostSignalRow,
  type RemoteSessionOperations,
  type RemoteSessionSnapshot,
} from "../lib/remote-session";

export type ViewerSession = {
  state: "requested" | "ringing" | "negotiating" | "connected" | "ended" | "failed";
  expiresAt: number;
  endReason?: string;
  controllerEpoch?: string;
  remoteInput: boolean;
  displays: DisplayInfo[];
};

type ViewerOperations = RemoteSessionOperations & {
  getSessionState?: () => Promise<ViewerSession>;
  claimController: (args: {
    sessionId: string;
    controllerEpoch: string;
    expectedControllerEpoch?: string;
  }) => Promise<{
    controllerEpoch: string;
  }>;
  getTurnCredentials: (args: { sessionId: string }) => Promise<{
    urls: string[];
    username: string;
    credential: string;
    expiresAt: number;
  } | null>;
};

export function RemoteViewer({ sessionId }: { sessionId: string }) {
  const sendSignal = useMutation(functions.signals.send);
  const endSession = useMutation(functions.sessions.end);
  const claimController = useMutation(functions.sessions.claimController);
  const getTurnCredentials = useAction(functions.sessions.turnCredentials);
  const convex = useConvex();
  const session = useQuery(functions.sessions.getState, { sessionId });
  const getSessionState = useCallback(
    () => convex.query(functions.sessions.getState, { sessionId }),
    [convex, sessionId],
  );
  const [hostSignalCursor, setHostSignalCursor] = useState(-1);
  useEffect(() => setHostSignalCursor(-1), [sessionId]);
  const incoming = useQuery(functions.signals.list, {
    sessionId,
    // The Convex query replays a bounded recent window by insertion time. The controller
    // deduplicates by generation and sequence, including across a native-agent restart.
    afterSequence: Math.max(-1, hostSignalCursor - 64),
  });

  const operations = useMemo(
    () => ({ sendSignal, endSession, claimController, getTurnCredentials, getSessionState }),
    [claimController, endSession, getSessionState, getTurnCredentials, sendSignal],
  );

  return (
    <RemoteViewerCore
      key={sessionId}
      sessionId={sessionId}
      session={session}
      incoming={incoming}
      hostSignalCursor={hostSignalCursor}
      onHostSignalCursor={setHostSignalCursor}
      operations={operations}
    />
  );
}

export function RemoteViewerCore({
  sessionId,
  session,
  incoming,
  hostSignalCursor = -1,
  onHostSignalCursor,
  operations,
}: {
  sessionId: string;
  session: ViewerSession | undefined;
  incoming: HostSignalRow[] | undefined;
  hostSignalCursor?: number;
  onHostSignalCursor?: (sequence: number) => void;
  operations: ViewerOperations;
}) {
  const videoRef = useRef<HTMLVideoElement>(null);
  const controllerRef = useRef<RemoteSessionController | null>(null);
  const controllerSessionRef = useRef<string | null>(null);
  const pendingControllerCleanupRef = useRef<{
    timer: number;
    sessionId: string;
    controller: RemoteSessionController | null;
  } | null>(null);
  const controllerClaimRef = useRef<{
    sessionId: string;
    promise: Promise<string>;
  } | null>(null);
  const restoredControllerEpochRef = useRef<string | undefined>(undefined);
  const controllerOwnershipLostRef = useRef(false);
  const incomingRef = useRef(incoming);
  incomingRef.current = incoming;
  const iceServersSessionRef = useRef<string | null>(null);
  const [iceServers, setIceServers] = useState<RTCIceServer[] | null>(null);
  const [ending, setEnding] = useState(false);
  const [endError, setEndError] = useState<string | null>(null);
  const [now, setNow] = useState(() => Date.now());
  const [selectedDisplay, setSelectedDisplay] = useState("");
  const [controllerRestoreNonce, setControllerRestoreNonce] = useState(0);
  const [snapshot, setSnapshot] = useState<RemoteSessionSnapshot>(() =>
    snapshotForSession(session),
  );
  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), 1_000);
    return () => window.clearInterval(timer);
  }, []);

  const sessionActive = Boolean(session && !isTerminalSession(session, now));
  const displays = session?.displays ?? [];
  const displaySignature = displays
    .map((display) =>
      [
        display.id,
        display.name,
        display.width,
        display.height,
        display.scaleFactor,
        display.primary,
      ].join("\u0000"),
    )
    .join("\u0001");
  const remoteInputEnabled = session?.remoteInput ?? false;
  useEffect(() => {
    iceServersSessionRef.current = null;
    setIceServers(null);
    setSelectedDisplay("");
    setEnding(false);
    setEndError(null);
    setSnapshot(snapshotForSession(session, now));
    setControllerRestoreNonce(0);
    restoredControllerEpochRef.current = undefined;
    controllerOwnershipLostRef.current = false;
  }, [sessionId]);

  useEffect(() => {
    if (!session) return;
    if (isTerminalSession(session, now)) {
      setSnapshot(snapshotForSession(session, now));
      const controller = controllerRef.current;
      if (controller) void controller.stop(session.endReason ?? "session ended", false);
    }
  }, [now, session]);

  useEffect(() => {
    const controller = controllerRef.current;
    if (
      !controller ||
      controllerSessionRef.current !== sessionId ||
      !session?.controllerEpoch ||
      session.controllerEpoch === controller.controllerEpochId
    ) {
      return;
    }
    // Convex propagates a replacement claim to the old page independently of WebRTC polling.
    // Close the old data channels as soon as that lease changes, while the native host also
    // revokes its input lane before asynchronous peer/media teardown.
    controllerOwnershipLostRef.current = true;
    controllerRef.current = null;
    controllerSessionRef.current = null;
    void controller.stop("controller replaced", false);
  }, [session?.controllerEpoch, sessionId]);

  useEffect(() => {
    if (!sessionActive) return;
    let cancelled = false;
    void operations
      .getTurnCredentials({ sessionId })
      .then((turn) => {
        if (cancelled) return;
        iceServersSessionRef.current = sessionId;
        setIceServers([
          { urls: "stun:stun.cloudflare.com:3478" },
          ...(turn
            ? [{ urls: turn.urls, username: turn.username, credential: turn.credential }]
            : []),
        ]);
      })
      .catch(() => {
        if (cancelled) return;
        iceServersSessionRef.current = sessionId;
        setIceServers([{ urls: "stun:stun.cloudflare.com:3478" }]);
      });
    return () => {
      cancelled = true;
    };
  }, [operations, sessionActive, sessionId]);

  useEffect(() => {
    const video = videoRef.current;

    // React Strict Mode probes effects by cleaning them up and immediately mounting them again.
    // Delay the destructive cleanup by one task so that probe is harmless, while a real unmount
    // still releases the peer. A session swap is not a
    // probe: release the old controller immediately and never let it own the new session's
    // signals. Teardown is deliberately non-terminal; the host detects an abandoned page, while
    // a refresh can claim a new controller epoch without racing an old end mutation.
    const pendingCleanup = pendingControllerCleanupRef.current;
    if (pendingCleanup && pendingCleanup.sessionId === sessionId) {
      window.clearTimeout(pendingCleanup.timer);
      pendingControllerCleanupRef.current = null;
    }
    if (pendingCleanup && pendingCleanup.sessionId !== sessionId) {
      window.clearTimeout(pendingCleanup.timer);
      pendingControllerCleanupRef.current = null;
      if (pendingCleanup.controller) {
        void pendingCleanup.controller.stop("controller disconnected", false);
      }
    }
    if (controllerClaimRef.current?.sessionId !== sessionId) {
      controllerClaimRef.current = null;
    }
    const existingController = controllerRef.current;
    if (existingController && controllerSessionRef.current !== sessionId) {
      if (pendingCleanup) window.clearTimeout(pendingCleanup.timer);
      pendingControllerCleanupRef.current = null;
      controllerRef.current = null;
      controllerSessionRef.current = null;
      void existingController.stop("controller disconnected", false);
    }
    if (controllerRef.current && controllerSessionRef.current === sessionId) {
      controllerRef.current.updateCapabilities(displays, remoteInputEnabled);
    }

    const scheduleControllerCleanup = (
      controller: RemoteSessionController | null,
      ownedSessionId: string,
    ) => {
      const timer = window.setTimeout(() => {
        const ownsPeer = controller
          ? controllerRef.current === controller && controllerSessionRef.current === ownedSessionId
          : controllerRef.current === null && controllerSessionRef.current === null;
        if (!ownsPeer) return;
        pendingControllerCleanupRef.current = null;
        controllerClaimRef.current = null;
        if (controller) {
          controllerRef.current = null;
          controllerSessionRef.current = null;
          void controller.stop("controller disconnected", false);
        }
      }, 0);
      pendingControllerCleanupRef.current = {
        timer,
        sessionId: ownedSessionId,
        controller,
      };
    };

    if (!sessionActive && controllerRef.current) {
      const controller = controllerRef.current;
      controllerRef.current = null;
      controllerSessionRef.current = null;
      void controller.stop(session?.endReason ?? "controller disconnected", false);
    }
    if (!sessionActive && controllerClaimRef.current?.sessionId === sessionId) {
      controllerClaimRef.current = null;
    }
    if (controllerOwnershipLostRef.current) return;
    if (
      !video ||
      !sessionActive ||
      !iceServers ||
      iceServersSessionRef.current !== sessionId ||
      controllerRef.current
    ) {
      if (controllerRef.current && controllerSessionRef.current === sessionId) {
        return () => scheduleControllerCleanup(controllerRef.current!, sessionId);
      }
      return;
    }
    if (controllerRef.current) {
      return () => scheduleControllerCleanup(controllerRef.current, sessionId);
    }

    const expectedControllerEpoch = restoredControllerEpochRef.current ?? session?.controllerEpoch;
    const claim =
      controllerClaimRef.current?.sessionId === sessionId
        ? controllerClaimRef.current.promise
        : (() => {
            const proposedEpoch = createControllerEpoch();
            const promise = operations
              .claimController({
                sessionId,
                controllerEpoch: proposedEpoch,
                ...(expectedControllerEpoch ? { expectedControllerEpoch } : {}),
              })
              .then((result) => result.controllerEpoch);
            controllerClaimRef.current = { sessionId, promise };
            return promise;
          })();
    let cancelled = false;
    let controller: RemoteSessionController | null = null;
    void claim
      .then((controllerEpoch) => {
        if (cancelled || controllerRef.current || !sessionActive) return;
        controllerOwnershipLostRef.current = false;
        restoredControllerEpochRef.current = undefined;
        if (controllerClaimRef.current?.promise === claim) controllerClaimRef.current = null;
        controller = new RemoteSessionController({
          sessionId,
          controllerEpoch,
          displays,
          remoteInputEnabled,
          iceServers,
          video,
          operations,
          getIceServers: async () => {
            try {
              const turn = await operations.getTurnCredentials({ sessionId });
              return [
                { urls: "stun:stun.cloudflare.com:3478" },
                ...(turn
                  ? [{ urls: turn.urls, username: turn.username, credential: turn.credential }]
                  : []),
              ];
            } catch {
              return [{ urls: "stun:stun.cloudflare.com:3478" }];
            }
          },
          revalidateController: operations.getSessionState
            ? async () => {
                const current = await operations.getSessionState!();
                return {
                  owned: current.controllerEpoch === controllerEpoch,
                  ...(current.controllerEpoch ? { controllerEpoch: current.controllerEpoch } : {}),
                };
              }
            : undefined,
          onPageRestore: (currentControllerEpoch) => {
            if (controllerRef.current !== controller) return;
            const pending = pendingControllerCleanupRef.current;
            if (pending) window.clearTimeout(pending.timer);
            pendingControllerCleanupRef.current = null;
            controllerOwnershipLostRef.current = false;
            restoredControllerEpochRef.current = currentControllerEpoch;
            controllerRef.current = null;
            controllerSessionRef.current = null;
            setControllerRestoreNonce((value) => value + 1);
          },
          onSnapshot: setSnapshot,
        });
        controllerRef.current = controller;
        controllerSessionRef.current = sessionId;
        const startPromise = controller.start();
        // The initial replay may already be in the reactive query when the claim resolves. Feed it
        // after the local offer has been published so an answer cannot be consumed before there is
        // an outstanding local offer to match it.
        void startPromise.then(() => {
          if (controllerRef.current === controller) {
            controller?.ingestHostSignals(incomingRef.current ?? []);
          }
        });
      })
      .catch(() => {
        if (cancelled) return;
        if (controllerClaimRef.current?.promise === claim) controllerClaimRef.current = null;
        controllerOwnershipLostRef.current = true;
        setSnapshot((current) => ({
          ...current,
          phase: "failed",
          reason: "controller claim failed",
        }));
      });
    return () => {
      cancelled = true;
      scheduleControllerCleanup(controller, sessionId);
    };
  }, [
    controllerRestoreNonce,
    displaySignature,
    iceServers,
    operations,
    remoteInputEnabled,
    sessionActive,
    sessionId,
    session?.controllerEpoch,
  ]);

  useEffect(() => {
    controllerRef.current?.ingestHostSignals(incoming ?? []);
    const latest = incoming?.reduce(
      (maximum, row) =>
        Math.max(maximum, Number.isSafeInteger(row.sequence) ? row.sequence : maximum),
      -1,
    );
    if (latest !== undefined && latest > hostSignalCursor) onHostSignalCursor?.(latest);
  }, [hostSignalCursor, incoming, onHostSignalCursor]);

  useEffect(() => {
    const available = session?.displays ?? [];
    if (available.length === 0) {
      if (selectedDisplay) setSelectedDisplay("");
      return;
    }
    if (!available.some((display) => display.id === selectedDisplay)) {
      setSelectedDisplay(
        available.find((display) => display.primary)?.id ?? available[0]?.id ?? "",
      );
    }
  }, [selectedDisplay, session?.displays]);

  async function leaveSession(): Promise<void> {
    if (ending) return;
    setEnding(true);
    setEndError(null);
    const controller = controllerRef.current;
    const controllerEpoch = controller?.controllerEpochId;
    await controller?.stop("ended by controller", false);
    try {
      await operations.endSession({
        sessionId,
        reason: "ended by controller",
        ...(controllerEpoch ? { controllerEpoch } : {}),
      });
      window.location.assign("/dashboard");
    } catch {
      setEndError("The stream is closed, but the session could not be finalized. Retry.");
      setEnding(false);
    }
  }

  function setRemoteControl(enabled: boolean): void {
    if (!session?.remoteInput) return;
    controllerRef.current?.setInputEnabled(enabled);
    if (!controllerRef.current) {
      setSnapshot((current) => ({ ...current, inputEnabled: enabled }));
    }
  }

  const status = statusLabel(snapshot, session);
  return (
    <main className="viewer">
      <header className="viewer-bar">
        <a
          className="viewer-back"
          href="/dashboard"
          onClick={(event) => {
            event.preventDefault();
            void leaveSession();
          }}
        >
          ← Devices
        </a>
        <div className="viewer-health" aria-live="polite">
          <span className={`viewer-state ${snapshot.phase}`}>
            <i aria-hidden="true" />
            {status}
          </span>
          {snapshot.metrics ? <small>{formatMetrics(snapshot.metrics)}</small> : null}
        </div>
        <div className="viewer-actions">
          {displays.length > 1 ? (
            <label className="display-picker">
              <span>Display</span>
              <select
                value={selectedDisplay}
                disabled={!snapshot.inputEnabled}
                onChange={(event) => {
                  if (controllerRef.current?.selectDisplay(event.target.value)) {
                    setSelectedDisplay(event.target.value);
                  }
                }}
              >
                {displays.map((display) => (
                  <option key={display.id} value={display.id}>
                    {display.name} ({display.width}×{display.height})
                  </option>
                ))}
              </select>
            </label>
          ) : null}
          <button
            type="button"
            title="Emergency release: Ctrl+Alt+Shift+Escape"
            aria-pressed={snapshot.inputEnabled}
            disabled={!session?.remoteInput}
            onClick={() => setRemoteControl(!snapshot.inputEnabled)}
          >
            {session?.remoteInput
              ? `Control: ${snapshot.inputEnabled ? "on" : "off"}`
              : "View only"}
          </button>
          <button type="button" onClick={() => void videoRef.current?.requestFullscreen()}>
            Fullscreen
          </button>
          <button
            className="danger"
            type="button"
            disabled={
              ending || (!endError && (snapshot.phase === "ended" || snapshot.phase === "failed"))
            }
            onClick={() => void leaveSession()}
          >
            {ending ? "Ending…" : endError ? "Retry end" : "End session"}
          </button>
        </div>
      </header>
      {endError ? (
        <p className="viewer-error" role="alert">
          {endError}
        </p>
      ) : null}
      <section className="viewer-stage" aria-label="Remote desktop">
        <video ref={videoRef} autoPlay playsInline tabIndex={0} disablePictureInPicture />
        {snapshot.phase !== "connected" || !snapshot.firstFrameReceived ? (
          <div className="viewer-overlay" role="status">
            {snapshot.phase !== "failed" && snapshot.phase !== "ended" ? (
              <span className="viewer-spinner" aria-hidden="true" />
            ) : null}
            <strong>{status}</strong>
            <small>
              {snapshot.phase === "connected" && !snapshot.firstFrameReceived
                ? "The connection is ready; waiting for the first video frame."
                : snapshot.phase === "reconnecting"
                  ? "The connection is recovering. Your input is released until the stream returns."
                  : snapshot.phase === "failed" || snapshot.phase === "ended"
                    ? (snapshot.reason ?? "The session is no longer available.")
                    : "Negotiating a direct connection, with relay fallback when needed."}
            </small>
          </div>
        ) : null}
        {snapshot.phase === "connected" && snapshot.firstFrameReceived ? (
          <p className="viewer-hint">Click the desktop to capture keyboard and pointer input.</p>
        ) : null}
      </section>
    </main>
  );
}

function isTerminalSession(session: ViewerSession | undefined, now = Date.now()): boolean {
  return Boolean(
    !session || session.state === "ended" || session.state === "failed" || session.expiresAt <= now,
  );
}

function snapshotForSession(
  session: ViewerSession | undefined,
  now = Date.now(),
): RemoteSessionSnapshot {
  if (!session) {
    return {
      phase: "negotiating",
      reason: "Loading session",
      reconnectAttempt: 0,
      iceGeneration: 0,
      inputEnabled: false,
      firstFrameReceived: false,
      metrics: null,
    };
  }
  if (session.state === "ended" || session.expiresAt <= now) {
    return {
      phase: "ended",
      reason: session?.endReason ?? "session expired",
      reconnectAttempt: 0,
      iceGeneration: 0,
      inputEnabled: false,
      firstFrameReceived: false,
      metrics: null,
    };
  }
  if (session.state === "failed") {
    return {
      phase: "failed",
      reason: session.endReason ?? "session failed",
      reconnectAttempt: 0,
      iceGeneration: 0,
      inputEnabled: false,
      firstFrameReceived: false,
      metrics: null,
    };
  }
  return {
    phase: "negotiating",
    reconnectAttempt: 0,
    iceGeneration: 0,
    inputEnabled: false,
    firstFrameReceived: false,
    metrics: null,
  };
}

function statusLabel(snapshot: RemoteSessionSnapshot, session: ViewerSession | undefined): string {
  if (!session) return "Loading session…";
  if (snapshot.phase === "ended")
    return `ended: ${snapshot.reason ?? session?.endReason ?? "session ended"}`;
  if (snapshot.phase === "failed") return "failed";
  if (snapshot.phase === "connected") {
    return snapshot.firstFrameReceived ? "Live" : "Connected · waiting for video";
  }
  if (snapshot.phase === "reconnecting") {
    return `reconnecting (${snapshot.reconnectAttempt}/3)`;
  }
  return "Negotiating";
}

function formatMetrics(metrics: NonNullable<RemoteSessionSnapshot["metrics"]>): string {
  return `${metrics.resolution} · ${metrics.fps} fps · ${formatBitrate(metrics.bitrateKbps)} · ${
    metrics.rttMs
  } ms · ${metrics.route} · ${metrics.packetsLost} lost · ${metrics.framesDropped} dropped`;
}

function formatBitrate(kbps: number): string {
  return kbps >= 1_000 ? `${(kbps / 1_000).toFixed(1)} Mbps` : `${kbps} kbps`;
}
