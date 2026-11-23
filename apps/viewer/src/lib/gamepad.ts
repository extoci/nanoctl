import type { ControlMessage, GamepadReport } from "@nanoctl/protocol";

const XINPUT_BUTTONS = [
  0x1000, 0x2000, 0x4000, 0x8000, 0x0100, 0x0200, 0, 0, 0x0020, 0x0010, 0x0040, 0x0080, 0x0001,
  0x0002, 0x0004, 0x0008, 0x0400,
];
const KEEPALIVE_MS = 500;
type InputPad = Pick<Gamepad, "axes" | "buttons" | "connected" | "id" | "index" | "mapping">;
type GamepadMessage = Extract<ControlMessage, { type: "gamepad" }>;
type DualShock4Pad = NonNullable<GamepadMessage["dualShock4"]>[number];
export type GamepadStatus = "waiting" | "connected" | "unsupported";

function isPlayStation(id: string): boolean {
  return (
    /\b(?:dualsense|dualshock\s*4?|playstation)\b|\bvendor:\s*054c\b/i.test(id) ||
    /^wireless controller(?: extended gamepad| gamepad)?$/i.test(id.trim())
  );
}

function clamp(value: number | undefined, min: number, max: number): number {
  return typeof value === "number" && Number.isFinite(value)
    ? Math.min(max, Math.max(min, value))
    : 0;
}

function axis(value: number | undefined, invert = false): number {
  const normalized = clamp(value, -1, 1) * (invert ? -1 : 1);
  return Math.round(normalized * (normalized < 0 ? 32768 : 32767)) || 0;
}

function report(pad: InputPad, slot: number): GamepadReport {
  const trigger = (index: number) => Math.round(clamp(pad.buttons[index]?.value, 0, 1) * 255);
  const buttons = XINPUT_BUTTONS.reduce(
    (mask, flag, index) => mask | (pad.buttons[index]?.pressed ? flag : 0),
    0,
  );
  return [
    slot,
    buttons,
    trigger(6),
    trigger(7),
    axis(pad.axes[0]),
    axis(pad.axes[1], true),
    axis(pad.axes[2]),
    axis(pad.axes[3], true),
  ];
}

export class GamepadForwarder {
  private readonly slots: (number | undefined)[] = Array.from({ length: 4 });
  private sentSlots = new Set<number>();
  private lastSent = JSON.stringify({ type: "gamepad", pads: [] });
  private sentAt = -Infinity;

  constructor(private readonly send: (message: GamepadMessage) => boolean) {}

  sample(pads: ArrayLike<InputPad | null>, now: number): GamepadStatus {
    const available = new Map<number, InputPad>();
    let unsupported = false;
    for (const pad of Array.from(pads)) {
      if (!pad?.connected) continue;
      if (pad.mapping === "standard") available.set(pad.index, pad);
      else unsupported = true;
    }
    for (let slot = 0; slot < this.slots.length; slot++) {
      const index = this.slots[slot];
      if (index !== undefined && !available.has(index)) this.slots[slot] = undefined;
    }
    for (const index of available.keys()) {
      if (this.slots.includes(index)) continue;
      const free = this.slots.findIndex(
        (current, slot) => current === undefined && !this.sentSlots.has(slot),
      );
      if (free === -1) break;
      this.slots[free] = index;
    }
    const reports: GamepadReport[] = [];
    const dualShock4: DualShock4Pad[] = [];
    this.slots.forEach((index, slot) => {
      const pad = index === undefined ? undefined : available.get(index);
      if (!pad) return;
      reports.push(report(pad, slot));
      if (isPlayStation(pad.id)) {
        dualShock4.push({
          slot,
          leftTriggerPressed: pad.buttons[6]?.pressed ?? false,
          rightTriggerPressed: pad.buttons[7]?.pressed ?? false,
          touchpadPressed: pad.buttons[17]?.pressed ?? false,
        });
      }
    });
    this.flush({ type: "gamepad", pads: reports, ...(dualShock4.length && { dualShock4 }) }, now);
    return available.size ? "connected" : unsupported ? "unsupported" : "waiting";
  }

  release(): void {
    this.flush({ type: "gamepad", pads: [] }, 0);
  }

  private flush(message: GamepadMessage, now: number): void {
    const { pads } = message;
    const state = JSON.stringify(message);
    if (state === this.lastSent && (!pads.length || now - this.sentAt < KEEPALIVE_MS)) return;
    if (this.send(message)) {
      this.lastSent = state;
      this.sentSlots = new Set(pads.map(([slot]) => slot));
      this.sentAt = now;
    }
  }
}
