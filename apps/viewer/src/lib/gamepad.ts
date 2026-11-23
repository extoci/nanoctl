import type { ControlMessage, GamepadReport } from "@nanoctl/protocol";

const XINPUT_BUTTONS = [
  0x1000, 0x2000, 0x4000, 0x8000, 0x0100, 0x0200, 0, 0, 0x0020, 0x0010, 0x0040, 0x0080, 0x0001,
  0x0002, 0x0004, 0x0008, 0x0400,
];
const KEEPALIVE_MS = 500;
type InputPad = Pick<Gamepad, "axes" | "buttons" | "connected" | "index" | "mapping">;
export type GamepadStatus = "waiting" | "connected" | "unsupported";

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
  private lastSent = "[]";
  private sentAt = -Infinity;

  constructor(
    private readonly send: (message: Extract<ControlMessage, { type: "gamepad" }>) => boolean,
  ) {}

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
    this.slots.forEach((index, slot) => {
      const pad = index === undefined ? undefined : available.get(index);
      if (pad) reports.push(report(pad, slot));
    });
    this.flush(reports, now);
    return available.size ? "connected" : unsupported ? "unsupported" : "waiting";
  }

  release(): void {
    this.flush([], 0);
  }

  private flush(pads: GamepadReport[], now: number): void {
    const state = JSON.stringify(pads);
    if (state === this.lastSent && (!pads.length || now - this.sentAt < KEEPALIVE_MS)) return;
    if (this.send({ type: "gamepad", pads })) {
      this.lastSent = state;
      this.sentSlots = new Set(pads.map(([slot]) => slot));
      this.sentAt = now;
    }
  }
}
