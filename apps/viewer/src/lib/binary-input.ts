import type { ControlMessage } from "@nanoctl/protocol";

const POINTER_MOVE = 1;
const GAMEPAD = 2;
const POINTER_BUTTON = 3;
const POINTER_WHEEL = 4;
const KEY = 5;
const TEXT = 6;
const RELEASE = 7;
const PING = 8;
const AUDIO = 9;
const DISPLAY = 10;
const GAMEPAD_HEARTBEAT = 11;
const GAMEPAD_SPARSE = 12;
const KEY_NAMED = 4;
const clamp01 = (value: number) => Math.min(1, Math.max(0, value));
const textEncoder = new TextEncoder();

const NAMED_KEYS: Record<string, number> = {
  Backspace: 1,
  Delete: 2,
  Enter: 3,
  NumpadEnter: 3,
  Escape: 4,
  Tab: 5,
  Space: 6,
  ArrowUp: 7,
  ArrowDown: 8,
  ArrowLeft: 9,
  ArrowRight: 10,
  Home: 11,
  End: 12,
  PageUp: 13,
  PageDown: 14,
  F1: 15,
  F2: 16,
  F3: 17,
  F4: 18,
  F5: 19,
  F6: 20,
  F7: 21,
  F8: 22,
  F9: 23,
  F10: 24,
  F11: 25,
  F12: 26,
  CapsLock: 27,
  PrintScreen: 28,
  Insert: 29,
  ShiftLeft: 30,
  ShiftRight: 30,
  ControlLeft: 31,
  ControlRight: 31,
  AltLeft: 32,
  AltRight: 32,
  MetaLeft: 33,
  MetaRight: 33,
};

function message(kind: number, length: number): DataView<ArrayBuffer> {
  const out = new DataView(new ArrayBuffer(length));
  out.setUint8(0, kind);
  return out;
}

function putPosition(out: DataView, offset: number, x: number, y: number): void {
  out.setUint16(offset, Math.round(clamp01(x) * 65535), true);
  out.setUint16(offset + 2, Math.round(clamp01(y) * 65535), true);
}

function utf8(value: string, maxBytes = 65535): Uint8Array | null {
  const bytes = textEncoder.encode(value);
  return bytes.length <= maxBytes ? bytes : null;
}

/** One-byte unchanged-gamepad liveness report; it carries no input state. */
export function encodeGamepadHeartbeat(): ArrayBuffer {
  return message(GAMEPAD_HEARTBEAT, 1).buffer;
}

type FullGamepad = Extract<ControlMessage, { type: "gamepad" }>;
export type SequencedGamepad = FullGamepad & { sequence: number };
export type SparseGamepad = Omit<FullGamepad, "type"> & {
  type: "gamepad-sparse";
  sequence: number;
};
export type GamepadStateInput = SequencedGamepad | SparseGamepad;

function encodeGamepadState(payload: GamepadStateInput): ArrayBuffer | null {
  if (payload.pads.length > 4) return null;
  const dualShock4 = new Map(payload.dualShock4?.map((pad) => [pad.slot, pad]) ?? []);
  const pads = [...payload.pads].sort((left, right) => left[0] - right[0]);
  const slots = new Set<number>();
  let mask = 0;
  for (const [slot] of pads) {
    if (slot > 3 || slots.has(slot)) return null;
    slots.add(slot);
    mask |= 1 << slot;
  }
  const ds4Flags = (slot: number) => {
    const ds4 = dualShock4.get(slot);
    return ds4
      ? 1 |
          (ds4.leftTriggerPressed ? 2 : 0) |
          (ds4.rightTriggerPressed ? 4 : 0) |
          (ds4.touchpadPressed ? 8 : 0)
      : 0;
  };
  if (payload.type === "gamepad-sparse") {
    // Sparse entries are still absolute per-slot state. Zero-valued fields are
    // omitted behind a field mask rather than delta-encoded, so any one record
    // fully repairs that slot even after earlier packet loss.
    const bytes = [GAMEPAD_SPARSE, payload.sequence & 0xff, mask];
    const u16 = (value: number) => bytes.push(value & 0xff, (value >> 8) & 0xff);
    for (const [slot, buttons, leftTrigger, rightTrigger, leftX, leftY, rightX, rightY] of pads) {
      const ds4 = dualShock4.has(slot);
      const fields =
        (buttons ? 1 : 0) |
        (leftTrigger ? 2 : 0) |
        (rightTrigger ? 4 : 0) |
        (leftX ? 8 : 0) |
        (leftY ? 16 : 0) |
        (rightX ? 32 : 0) |
        (rightY ? 64 : 0) |
        (ds4 ? 128 : 0);
      bytes.push(fields);
      if (buttons) {
        let compact = buttons;
        do {
          const byte = compact & 0x7f;
          compact >>= 7;
          bytes.push(compact ? byte | 0x80 : byte);
        } while (compact);
      }
      if (leftTrigger) bytes.push(leftTrigger);
      if (rightTrigger) bytes.push(rightTrigger);
      for (const axis of [leftX, leftY, rightX, rightY]) if (axis) u16(axis & 0xffff);
      if (ds4) bytes.push(ds4Flags(slot));
    }
    return new Uint8Array(bytes).buffer;
  }
  const out = message(GAMEPAD, 3 + pads.length * 13);
  out.setUint8(1, payload.sequence & 0xff);
  out.setUint8(2, mask);
  pads.forEach((pad, index) => {
    const offset = 3 + index * 13;
    const [slot, buttons, leftTrigger, rightTrigger, leftX, leftY, rightX, rightY] = pad;
    out.setUint16(offset, buttons, true);
    out.setUint8(offset + 2, leftTrigger);
    out.setUint8(offset + 3, rightTrigger);
    out.setInt16(offset + 4, leftX, true);
    out.setInt16(offset + 6, leftY, true);
    out.setInt16(offset + 8, rightX, true);
    out.setInt16(offset + 10, rightY, true);
    out.setUint8(offset + 12, ds4Flags(slot));
  });
  return out.buffer;
}

/**
 * Version-3 compact input wire format. Input datagram channels already provide
 * message boundaries, so the kind byte is the only framing overhead; all
 * integer and floating-point fields are little-endian.
 */
export function encodeBinaryInput(payload: ControlMessage | GamepadStateInput): ArrayBuffer | null {
  if (payload.type === "pointer" && payload.action === "move") {
    const out = message(POINTER_MOVE, 5);
    putPosition(out, 1, payload.x, payload.y);
    return out.buffer;
  }
  if (payload.type === "pointer" && (payload.action === "down" || payload.action === "up")) {
    const out = message(POINTER_BUTTON, 6);
    out.setUint8(1, (payload.action === "up" ? 0x80 : 0) | (payload.button ?? 0));
    putPosition(out, 2, payload.x, payload.y);
    return out.buffer;
  }
  if (payload.type === "pointer" && payload.action === "wheel") {
    const significantF64 = (value: number) => {
      const view = new DataView(new ArrayBuffer(8));
      view.setFloat64(0, value, false);
      const bytes = [...new Uint8Array(view.buffer)];
      while (bytes.length && bytes[bytes.length - 1] === 0) bytes.pop();
      return bytes;
    };
    const deltaX = significantF64(payload.deltaX ?? 0);
    const deltaY = significantF64(payload.deltaY ?? 0);
    const out = message(POINTER_WHEEL, 6 + deltaX.length + deltaY.length);
    putPosition(out, 1, payload.x, payload.y);
    out.setUint8(5, deltaX.length | (deltaY.length << 4));
    new Uint8Array(out.buffer, 6, deltaX.length).set(deltaX);
    new Uint8Array(out.buffer, 6 + deltaX.length, deltaY.length).set(deltaY);
    return out.buffer;
  }
  if (payload.type === "key") {
    const flags = (payload.action === "up" ? 1 : 0) | (payload.repeat ? 2 : 0);
    const named = NAMED_KEYS[payload.code];
    if (named) {
      const out = message(KEY, 3);
      out.setUint8(1, flags | KEY_NAMED);
      out.setUint8(2, named);
      return out.buffer;
    }
    const key = utf8(payload.key, 32);
    if (!key?.length) return null;
    const out = message(KEY, 2 + key.length);
    out.setUint8(1, flags);
    new Uint8Array(out.buffer, 2, key.length).set(key);
    return out.buffer;
  }
  if (payload.type === "text") {
    const text = utf8(payload.text);
    if (!text) return null;
    const out = message(TEXT, 3 + text.length);
    out.setUint16(1, text.length, true);
    new Uint8Array(out.buffer, 3, text.length).set(text);
    return out.buffer;
  }
  if (payload.type === "release") return message(RELEASE, 1).buffer;
  if (payload.type === "ping" || payload.type === "pong") return message(PING, 1).buffer;
  if (payload.type === "audio") {
    const out = message(AUDIO, 2);
    out.setUint8(1, payload.enabled ? 1 : 0);
    return out.buffer;
  }
  if (payload.type === "display") {
    const display = utf8(payload.displayId, 255);
    if (!display) return null;
    const out = message(DISPLAY, 2 + display.length);
    out.setUint8(1, display.length);
    new Uint8Array(out.buffer, 2, display.length).set(display);
    return out.buffer;
  }
  if (payload.type !== "gamepad" && payload.type !== "gamepad-sparse") return null;
  if (!("sequence" in payload)) return null;
  return encodeGamepadState(payload);
}
