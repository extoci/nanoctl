"use client";

import { useMemo, useRef, useState } from "react";
import { RemoteViewerCore, type ViewerSession } from "./remote-viewer";

const DISPLAYS = [
  {
    id: "display-primary",
    name: "Primary",
    width: 2560,
    height: 1440,
    scaleFactor: 1,
    primary: true,
  },
  {
    id: "display-secondary",
    name: "Secondary",
    width: 1920,
    height: 1080,
    scaleFactor: 1,
    primary: false,
  },
];

function appendFixtureEvent(value: unknown): void {
  const target = window as typeof window & { __nanoctlViewerEvents?: unknown[] };
  target.__nanoctlViewerEvents ??= [];
  target.__nanoctlViewerEvents.push(value);
}

export function E2eRemoteViewerFixture({ terminal }: { terminal: boolean }) {
  const [mounted, setMounted] = useState(true);
  const [sessionId, setSessionId] = useState("viewer-fixture-a");
  const [incoming, setIncoming] = useState<{ sequence: number; envelope: string }[]>([]);
  const hostSequenceRef = useRef(0);
  const [sessionState, setSessionState] = useState<ViewerSession["state"]>(
    terminal ? "ended" : "requested",
  );
  const session = useMemo<ViewerSession>(
    () => ({
      state: sessionState,
      expiresAt: Date.now() + 600_000,
      endReason: terminal ? "host stopped" : undefined,
      remoteInput: true,
      displays: DISPLAYS,
    }),
    [sessionState, terminal],
  );
  const operations = useMemo(
    () => ({
      claimController: async (args: {
        sessionId: string;
        controllerEpoch: string;
        expectedControllerEpoch?: string;
      }) => {
        appendFixtureEvent({ operation: "claim", ...args });
        return { controllerEpoch: args.controllerEpoch };
      },
      sendSignal: async (args: { sessionId: string; envelope: string }) => {
        appendFixtureEvent({ operation: "signal", ...args });
        try {
          const offer = JSON.parse(args.envelope) as {
            version?: unknown;
            sessionId?: unknown;
            controllerEpoch?: unknown;
            generation?: unknown;
            sender?: unknown;
            payload?: { type?: unknown };
          };
          if (
            offer.version !== 2 ||
            offer.sender !== "controller" ||
            offer.payload?.type !== "offer" ||
            typeof offer.sessionId !== "string" ||
            typeof offer.controllerEpoch !== "string" ||
            !Number.isSafeInteger(offer.generation)
          ) {
            return null;
          }
          const sequence = hostSequenceRef.current++;
          setIncoming((current) => [
            ...current,
            {
              sequence,
              envelope: JSON.stringify({
                version: 2,
                sessionId: offer.sessionId,
                controllerEpoch: offer.controllerEpoch,
                hostEpoch: "fixture-host",
                generation: offer.generation,
                sequence,
                sender: "host",
                sentAt: Date.now(),
                payload: { type: "answer", sdp: "v=0\r\n" },
              }),
            },
          ]);
        } catch {
          // The controller's own signal validation covers malformed fixture payloads.
        }
        return null;
      },
      endSession: async (args: { sessionId: string; reason: string; controllerEpoch?: string }) => {
        appendFixtureEvent({ operation: "end", ...args });
        return null;
      },
      getTurnCredentials: async () => null,
    }),
    [],
  );

  return (
    <>
      <button className="fixture-unmount" type="button" onClick={() => setMounted(false)}>
        Unmount viewer
      </button>
      <button
        className="fixture-reopen"
        type="button"
        onClick={() =>
          setSessionId((current) =>
            current.endsWith("-a") ? "viewer-fixture-b" : "viewer-fixture-a",
          )
        }
      >
        Reopen viewer
      </button>
      {!terminal ? (
        <button
          className="fixture-state"
          type="button"
          onClick={() =>
            setSessionState((current) => (current === "requested" ? "negotiating" : "connected"))
          }
        >
          Advance session state
        </button>
      ) : null}
      {mounted ? (
        <RemoteViewerCore
          sessionId={sessionId}
          session={session}
          incoming={incoming}
          operations={operations}
        />
      ) : (
        <p>Viewer unmounted</p>
      )}
    </>
  );
}
