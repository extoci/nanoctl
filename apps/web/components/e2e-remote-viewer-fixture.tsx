"use client";

import { useMemo, useState } from "react";
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
  const session = useMemo<ViewerSession>(
    () => ({
      state: terminal ? "ended" : "connected",
      expiresAt: Date.now() + 600_000,
      endReason: terminal ? "host stopped" : undefined,
      displays: DISPLAYS,
    }),
    [terminal],
  );
  const operations = useMemo(
    () => ({
      sendSignal: async (args: { sessionId: string; envelope: string }) => {
        appendFixtureEvent({ operation: "signal", ...args });
        return null;
      },
      endSession: async (args: { sessionId: string; reason: string }) => {
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
      {mounted ? (
        <RemoteViewerCore
          sessionId={sessionId}
          session={session}
          incoming={[]}
          operations={operations}
        />
      ) : (
        <p>Viewer unmounted</p>
      )}
    </>
  );
}
