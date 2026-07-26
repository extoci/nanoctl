"use client";

import { useAction, useMutation, useQuery } from "convex/react";
import { functions } from "../lib/convex";
import { signOut } from "../lib/shoo";
import { DeviceDashboardView } from "./device-dashboard-view";

export function DeviceDashboard() {
  const devices = useQuery(functions.devices.list, {});
  const auditEvents = useQuery(functions.audit.listRecent, {});
  const createPairingCode = useAction(functions.devices.createPairingCode);
  const createSession = useMutation(functions.sessions.create);
  const renameDevice = useMutation(functions.devices.rename);
  const removeDevice = useMutation(functions.devices.remove);

  return (
    <DeviceDashboardView
      devices={devices}
      auditEvents={auditEvents}
      operations={{
        createPairingCode: () => createPairingCode({}),
        createSession: (deviceId) => createSession({ deviceId }),
        renameDevice: async (deviceId, name) => {
          await renameDevice({ deviceId, name });
        },
        removeDevice: async (deviceId) => {
          await removeDevice({ deviceId });
        },
        signOut,
      }}
    />
  );
}
