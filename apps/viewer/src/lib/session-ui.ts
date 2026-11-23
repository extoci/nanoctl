import type { ControlMessage } from "@nanoctl/protocol";
import type { RemoteSessionMetrics } from "./remote-session";
import { modifierKeys, type Modifier } from "./soft-keyboard";

export type Quality = "good" | "fair" | "poor";

export function connectionQuality(metrics: RemoteSessionMetrics | null): Quality | null {
  if (!metrics) return null;
  const fps = metrics.fps ?? Infinity;
  const rttMs = metrics.rttMs ?? 0;
  if (rttMs > 150 || fps < 10) return "poor";
  if (rttMs > 60 || fps < 24 || metrics.route === "relay") return "fair";
  return "good";
}

export type KeyCombo = {
  label: string;
  modifiers: Modifier[];
  key: string;
  code: string;
};

export function keyCombos(platform: string | undefined): KeyCombo[] {
  if (platform === "macos")
    return [
      { label: "Cmd+Tab", modifiers: ["Meta"], key: "Tab", code: "Tab" },
      { label: "Cmd+Space", modifiers: ["Meta"], key: " ", code: "Space" },
      { label: "Cmd+Q", modifiers: ["Meta"], key: "q", code: "KeyQ" },
      {
        label: "Screenshot",
        modifiers: ["Meta", "Shift"],
        key: "3",
        code: "Digit3",
      },
      {
        label: "Lock screen",
        modifiers: ["Control", "Meta"],
        key: "q",
        code: "KeyQ",
      },
    ];
  const meta = platform === "windows" ? "Win" : "Super";
  return [
    { label: "Alt+Tab", modifiers: ["Alt"], key: "Tab", code: "Tab" },
    { label: `${meta} key`, modifiers: [], key: "Meta", code: "MetaLeft" },
    { label: "Alt+F4", modifiers: ["Alt"], key: "F4", code: "F4" },
    {
      label: "Print Screen",
      modifiers: [],
      key: "PrintScreen",
      code: "PrintScreen",
    },
    { label: "Lock screen", modifiers: ["Meta"], key: "l", code: "KeyL" },
  ];
}

export function comboMessages(combo: KeyCombo): ControlMessage[] {
  const down: ControlMessage[] = [];
  const up: ControlMessage[] = [];
  let mask = 0;
  for (const modifier of combo.modifiers) {
    const { code, bit } = modifierKeys[modifier];
    mask |= bit;
    down.push({
      type: "key",
      action: "down",
      code,
      key: modifier,
      modifiers: mask,
      repeat: false,
    });
    up.unshift({
      type: "key",
      action: "up",
      code,
      key: modifier,
      modifiers: mask & ~bit,
      repeat: false,
    });
  }
  const key = {
    code: combo.code,
    key: combo.key,
    modifiers: mask,
    repeat: false,
  } as const;
  return [
    ...down,
    { type: "key", action: "down", ...key },
    { type: "key", action: "up", ...key },
    ...up,
  ];
}

export type ConnectStep = {
  label: string;
  state: "done" | "active" | "pending";
};

export function connectSteps(
  socketOnline: boolean,
  phase: string,
  firstFrame: boolean,
): ConnectStep[] {
  const reached = [socketOnline, socketOnline && phase === "connected", firstFrame];
  const labels = ["Reaching host over tailnet", "Securing media connection", "Receiving screen"];
  const active = reached.indexOf(false);
  return labels.map((label, index) => ({
    label,
    state: reached[index] ? "done" : index === active ? "active" : "pending",
  }));
}
