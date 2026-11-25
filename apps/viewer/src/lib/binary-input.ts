import type { ControlMessage } from "@nanoctl/protocol";

const MAGIC = 0x4e;
const VERSION = 1;
const VERSION_2 = 2;
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
const clamp01 = (value: number) => Math.min(1, Math.max(0, value));
const textEncoder = new TextEncoder();

function header(version: number, kind: number, length: number): DataView<ArrayBuffer> {
  const out = new DataView(new ArrayBuffer(length));
  out.setUint8(0, MAGIC);
  out.setUint8(1, version);
  out.setUint8(2, kind);
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

/** Three-byte unchanged-gamepad liveness report; it carries no input state. */
export function encodeGamepadHeartbeat(): ArrayBuffer {
  return header(VERSION_2, GAMEPAD_HEARTBEAT, 3).buffer;
}

/**
 * Compact input wire format. Version-1 pointer moves are seven bytes and
 * gamepad reports are four bytes plus fourteen per occupied slot. Version-2
 * kinds cover the discrete input messages that otherwise pay JSON overhead;
 * all integer fields are little-endian.
 */
export function encodeBinaryInput(payload: ControlMessage): ArrayBuffer | null {
  if (payload.type === "pointer" && payload.action === "move") {
    const out = header(VERSION, POINTER_MOVE, 7);
    putPosition(out, 3, payload.x, payload.y);
    return out.buffer;
  }
  if (payload.type === "pointer" && (payload.action === "down" || payload.action === "up")) {
    const out = header(VERSION_2, POINTER_BUTTON, 8);
    out.setUint8(3, (payload.action === "up" ? 0x80 : 0) | (payload.button ?? 0));
    putPosition(out, 4, payload.x, payload.y);
    return out.buffer;
  }
  if (payload.type === "pointer" && payload.action === "wheel") {
    const out = header(VERSION_2, POINTER_WHEEL, 23);
    putPosition(out, 3, payload.x, payload.y);
    out.setFloat64(7, payload.deltaX ?? 0, true);
    out.setFloat64(15, payload.deltaY ?? 0, true);
    return out.buffer;
  }
  if (payload.type === "key") {
    const code = utf8(payload.code, 255);
    const key = utf8(payload.key, 255);
    if (!code || !key) return null;
    const out = header(VERSION_2, KEY, 6 + code.length + key.length);
    out.setUint8(3, (payload.action === "up" ? 1 : 0) | (payload.repeat ? 2 : 0));
    out.setUint8(4, code.length);
    out.setUint8(5, key.length);
    new Uint8Array(out.buffer, 6, code.length).set(code);
    new Uint8Array(out.buffer, 6 + code.length, key.length).set(key);
    return out.buffer;
  }
  if (payload.type === "text") {
    const text = utf8(payload.text);
    if (!text) return null;
    const out = header(VERSION_2, TEXT, 5 + text.length);
    out.setUint16(3, text.length, true);
    new Uint8Array(out.buffer, 5, text.length).set(text);
    return out.buffer;
  }
  if (payload.type === "release") return header(VERSION_2, RELEASE, 3).buffer;
  if (payload.type === "ping" || payload.type === "pong") {
    const out = header(VERSION_2, PING, 19);
    out.setBigUint64(3, BigInt(payload.nonce), true);
    out.setBigUint64(11, BigInt(payload.sentAt), true);
    return out.buffer;
  }
  if (payload.type === "audio") {
    const out = header(VERSION_2, AUDIO, 4);
    out.setUint8(3, payload.enabled ? 1 : 0);
    return out.buffer;
  }
  if (payload.type === "display") {
    const display = utf8(payload.displayId, 255);
    if (!display) return null;
    const out = header(VERSION_2, DISPLAY, 4 + display.length);
    out.setUint8(3, display.length);
    new Uint8Array(out.buffer, 4, display.length).set(display);
    return out.buffer;
  }
  if (payload.type !== "gamepad" || payload.pads.length > 4) return null;
  const dualShock4 = new Map(payload.dualShock4?.map((pad) => [pad.slot, pad]) ?? []);
  const out = header(VERSION, GAMEPAD, 4 + payload.pads.length * 14);
  out.setUint8(3, payload.pads.length);
  payload.pads.forEach((pad, index) => {
    const offset = 4 + index * 14;
    const [slot, buttons, leftTrigger, rightTrigger, leftX, leftY, rightX, rightY] = pad;
    out.setUint8(offset, slot);
    out.setUint16(offset + 1, buttons, true);
    out.setUint8(offset + 3, leftTrigger);
    out.setUint8(offset + 4, rightTrigger);
    out.setInt16(offset + 5, leftX, true);
    out.setInt16(offset + 7, leftY, true);
    out.setInt16(offset + 9, rightX, true);
    out.setInt16(offset + 11, rightY, true);
    const ds4 = dualShock4.get(slot);
    out.setUint8(
      offset + 13,
      ds4
        ? 1 |
            (ds4.leftTriggerPressed ? 2 : 0) |
            (ds4.rightTriggerPressed ? 4 : 0) |
            (ds4.touchpadPressed ? 8 : 0)
        : 0,
    );
  });
  return out.buffer;
}
