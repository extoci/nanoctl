import type { ControlMessage, GamepadReport } from "@nanoctl/protocol";

const XINPUT_BUTTONS = [
  0x1000, 0x2000, 0x4000, 0x8000, 0x0100, 0x0200, 0, 0, 0x0020, 0x0010, 0x0040, 0x0080, 0x0001,
  0x0002, 0x0004, 0x0008, 0x0400,
];
const MAX_SLOTS = 4;
/** Every poll within this window after a change repeats the snapshot to repair loss. */
const REPEAT_MS = 100;
/**
 * Unchanged snapshots are re-sent at this interval once the repeat window ends.
 * The host neutralizes pads after 150 ms of silence, so two consecutive lost
 * keepalives are still tolerated.
 */
const KEEPALIVE_MS = 50;
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

function sameMessage(left: GamepadMessage, right: GamepadMessage): boolean {
  const samePads =
    left.pads.length === right.pads.length &&
    left.pads.every((report, index) =>
      report.every((value, field) => value === right.pads[index]?.[field]),
    );
  const leftDs4 = left.dualShock4 ?? [];
  const rightDs4 = right.dualShock4 ?? [];
  return (
    samePads &&
    leftDs4.length === rightDs4.length &&
    leftDs4.every((pad, index) => {
      const other = rightDs4[index];
      return (
        pad.slot === other?.slot &&
        pad.leftTriggerPressed === other.leftTriggerPressed &&
        pad.rightTriggerPressed === other.rightTriggerPressed &&
        pad.touchpadPressed === other.touchpadPressed
      );
    })
  );
}

/**
 * A slot is one virtual controller on the host, tracked by browser pad identity rather than by
 * position: `navigator.getGamepads()` renumbers pads as devices come and go, so identity keeps a
 * pad that re-enumerates on the same host controller instead of unplugging and replugging it.
 */
type TrackedPad = { index: number; id: string };

/**
 * Maps browser gamepads onto host-side virtual controller slots and publishes their state.
 *
 * Slots that go silent are released by omission: a report list without a slot detaches that slot's
 * virtual controller on the host. `release` instead zeroes the occupied slots — the controllers
 * stay plugged in and go idle — which is what a transient disconnect or window blur wants.
 * `detach` is the opposite and is reserved for an explicit "gamepad off"/session end.
 */
export class GamepadForwarder {
  private readonly slots: (TrackedPad | undefined)[] = Array.from({ length: MAX_SLOTS });
  private sentSlots = new Set<number>();
  private lastSentEmpty = true;
  private lastSent: GamepadMessage | null = null;
  private changedAt = -Infinity;
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
    const unclaimed = new Map(available);
    for (let slot = 0; slot < this.slots.length; slot++) {
      const tracked = this.slots[slot];
      if (!tracked) continue;
      const current = unclaimed.get(tracked.index);
      if (current?.id === tracked.id) {
        unclaimed.delete(tracked.index);
        continue;
      }
      const moved = [...unclaimed.values()].find((pad) => pad.id === tracked.id);
      if (moved) {
        this.slots[slot] = { index: moved.index, id: moved.id };
        unclaimed.delete(moved.index);
      } else {
        this.slots[slot] = undefined;
      }
    }
    for (const pad of unclaimed.values()) {
      const free = this.slots.findIndex(
        (tracked, slot) => tracked === undefined && !this.sentSlots.has(slot),
      );
      if (free === -1) break;
      this.slots[free] = { index: pad.index, id: pad.id };
    }
    const reports: GamepadReport[] = [];
    const dualShock4: DualShock4Pad[] = [];
    this.slots.forEach((tracked, slot) => {
      const pad = tracked === undefined ? undefined : available.get(tracked.index);
      if (!pad || !tracked) return;
      reports.push(report(pad, slot));
      if (isPlayStation(tracked.id)) {
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

  /** Zero every occupied slot without detaching it: the host keeps an idle controller. */
  release(): void {
    const pads: GamepadReport[] = [];
    const dualShock4: DualShock4Pad[] = [];
    this.slots.forEach((tracked, slot) => {
      if (!tracked) return;
      pads.push([slot, 0, 0, 0, 0, 0, 0, 0]);
      if (isPlayStation(tracked.id)) {
        dualShock4.push({
          slot,
          leftTriggerPressed: false,
          rightTriggerPressed: false,
          touchpadPressed: false,
        });
      }
    });
    this.flush({ type: "gamepad", pads, ...(dualShock4.length && { dualShock4 }) }, 0);
  }

  /** Tell the host to unplug every slot and forget the pad-to-slot mapping. */
  detach(): void {
    this.flush({ type: "gamepad", pads: [] }, 0);
    this.slots.fill(undefined);
    this.sentSlots.clear();
  }

  private flush(message: GamepadMessage, now: number): void {
    const { pads } = message;
    // Active snapshots are full state on an unordered lossy lane. A change is
    // repeated on every poll for REPEAT_MS, so one lost press or release is
    // still repaired on the next frame; after that an unchanged pad only needs
    // a keepalive. Empty detach snapshots are reliable one-shot transitions
    // and stay deduplicated.
    if (!pads.length && this.lastSentEmpty) return;
    if (pads.length) {
      if (!this.lastSent || !sameMessage(message, this.lastSent)) this.changedAt = now;
      else if (now - this.changedAt >= REPEAT_MS && now - this.sentAt < KEEPALIVE_MS) return;
    }
    if (this.send(message)) {
      this.lastSentEmpty = !pads.length;
      this.lastSent = pads.length ? structuredClone(message) : null;
      this.sentAt = now;
      this.sentSlots = new Set(pads.map(([slot]) => slot));
    }
  }
}
