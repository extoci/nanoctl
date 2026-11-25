import type { ControlMessage, GamepadReport } from "@nanoctl/protocol";
import type { GamepadStateInput } from "./binary-input";

const XINPUT_BUTTONS = [
  0x1000, 0x2000, 0x4000, 0x8000, 0x0100, 0x0200, 0, 0, 0x0020, 0x0010, 0x0040, 0x0080, 0x0001,
  0x0002, 0x0004, 0x0008, 0x0400,
];
const MAX_SLOTS = 4;
/** Every poll within this window after a change repeats the snapshot to repair loss. */
const REPEAT_MS = 100;
/**
 * Unchanged pads send a three-byte heartbeat at this interval once the repeat
 * window ends. The host neutralizes pads after 225 ms of silence, so two
 * consecutive lost heartbeats are still tolerated.
 */
const KEEPALIVE_MS = 75;
/** Full snapshots still refresh periodically so loss cannot wedge stick/button state. */
const FULL_REFRESH_MS = 225;
type InputPad = Pick<Gamepad, "axes" | "buttons" | "connected" | "id" | "index" | "mapping">;
type GamepadMessage = Extract<ControlMessage, { type: "gamepad" }>;
type GamepadHeartbeat = { type: "gamepad-heartbeat" };
type GamepadWireMessage = GamepadStateInput | GamepadHeartbeat;
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

function slotStates(message: GamepadMessage) {
  const ds4 = new Map(message.dualShock4?.map((pad) => [pad.slot, pad]) ?? []);
  return new Map(
    message.pads.map((report) => {
      const slot = report[0];
      const extension = ds4.get(slot);
      return [
        slot,
        {
          report,
          flags: extension
            ? 1 |
              (extension.leftTriggerPressed ? 2 : 0) |
              (extension.rightTriggerPressed ? 4 : 0) |
              (extension.touchpadPressed ? 8 : 0)
            : 0,
        },
      ];
    }),
  );
}

function sameSlot(
  left: { report: GamepadReport; flags: number } | undefined,
  right: { report: GamepadReport; flags: number } | undefined,
): boolean {
  return (
    left?.flags === right?.flags &&
    !!left === !!right &&
    (!left ||
      !right ||
      left.report.every((value, field) => field === 0 || value === right.report[field]))
  );
}

function sameMessage(left: GamepadMessage, right: GamepadMessage): boolean {
  const leftSlots = slotStates(left);
  const rightSlots = slotStates(right);
  return (
    leftSlots.size === rightSlots.size &&
    [...leftSlots].every(([slot, state]) => sameSlot(state, rightSlots.get(slot)))
  );
}

function changedSlots(
  baseline: GamepadMessage,
  current: GamepadMessage,
): { topology: boolean; slots: Set<number> } {
  const before = slotStates(baseline);
  const after = slotStates(current);
  const slots = new Set<number>();
  let topology = before.size !== after.size;
  for (const slot of new Set([...before.keys(), ...after.keys()])) {
    if (!before.has(slot) || !after.has(slot)) topology = true;
    if (!sameSlot(before.get(slot), after.get(slot))) slots.add(slot);
  }
  return { topology, slots };
}

function subset(message: GamepadMessage, slots: Set<number>): GamepadMessage {
  return {
    type: "gamepad",
    pads: message.pads.filter(([slot]) => slots.has(slot)),
    ...(message.dualShock4?.length && {
      dualShock4: message.dualShock4.filter(({ slot }) => slots.has(slot)),
    }),
  };
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
  private readonly slots: (TrackedPad | undefined)[] = Array.from({
    length: MAX_SLOTS,
  });
  private sentSlots = new Set<number>();
  private lastSentEmpty = true;
  private lastSent: GamepadMessage | null = null;
  private baseline: GamepadMessage | null = null;
  private sequence = 0;
  private changedAt = -Infinity;
  private sentAt = -Infinity;
  private fullAt = -Infinity;

  constructor(
    private readonly send: (message: GamepadWireMessage) => boolean,
    private readonly timing: {
      repeatMs: number;
      keepaliveMs: number;
      heartbeat: boolean;
      sparse?: boolean;
    } = {
      repeatMs: REPEAT_MS,
      keepaliveMs: KEEPALIVE_MS,
      heartbeat: true,
    },
  ) {}

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
    this.flush(
      {
        type: "gamepad",
        pads: reports,
        ...(dualShock4.length && { dualShock4 }),
      },
      now,
    );
    return available.size ? "connected" : unsupported ? "unsupported" : "waiting";
  }

  /** Force the next sample to publish full state after a transport or input restart. */
  resume(): void {
    this.lastSent = null;
    this.baseline = null;
    this.changedAt = -Infinity;
    this.sentAt = -Infinity;
    this.fullAt = -Infinity;
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

  private sendState(message: GamepadMessage, sparse: boolean): boolean {
    const wire = {
      ...message,
      type: sparse ? "gamepad-sparse" : "gamepad",
      sequence: this.sequence,
    } as GamepadStateInput;
    if (!this.send(wire)) return false;
    this.sequence = (this.sequence + 1) & 0xff;
    return true;
  }

  private flush(message: GamepadMessage, now: number): void {
    const { pads } = message;
    // Active state uses an unordered lossy lane. Full snapshots rebase the
    // receiver; sparse snapshots carry only slots changed since that baseline
    // (plus slots in the latest transition), so every received sparse packet
    // repairs earlier sparse loss without giving up exact controller state.
    if (!pads.length && this.lastSentEmpty) return;
    if (pads.length) {
      if (!this.lastSent || !sameMessage(message, this.lastSent)) this.changedAt = now;
      else if (
        now - this.changedAt >= this.timing.repeatMs &&
        now - this.sentAt < this.timing.keepaliveMs
      )
        return;
      else if (
        this.timing.heartbeat &&
        now - this.changedAt >= this.timing.repeatMs &&
        now - this.fullAt < FULL_REFRESH_MS
      ) {
        if (this.send({ type: "gamepad-heartbeat" })) this.sentAt = now;
        return;
      }
    }
    const diff = this.baseline
      ? changedSlots(this.baseline, message)
      : { topology: true, slots: new Set<number>() };
    const full =
      !pads.length ||
      !this.baseline ||
      diff.topology ||
      this.timing.sparse === false ||
      now - this.fullAt >= FULL_REFRESH_MS;
    let outgoing = message;
    if (!full) {
      const slots = new Set(diff.slots);
      if (this.lastSent) {
        for (const slot of changedSlots(this.lastSent, message).slots) slots.add(slot);
      }
      if (!slots.size) {
        // The first full snapshot is repeated through the loss-repair window
        // too; after that this branch is unreachable because either the state
        // is unchanged (heartbeat/throttled) or a transition names its slots.
        for (const [slot] of message.pads) slots.add(slot);
      }
      outgoing = subset(message, slots);
    }
    if (this.sendState(outgoing, !full)) {
      this.lastSentEmpty = !pads.length;
      this.lastSent = pads.length ? structuredClone(message) : null;
      if (full) this.baseline = structuredClone(message);
      this.sentAt = now;
      this.fullAt = full ? now : this.fullAt;
      this.sentSlots = new Set(pads.map(([slot]) => slot));
    }
  }
}
