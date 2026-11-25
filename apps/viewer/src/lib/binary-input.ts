import type { ControlMessage } from "@nanoctl/protocol";

const MAGIC = 0x4e;
const VERSION = 1;
const POINTER_MOVE = 1;
const GAMEPAD = 2;
const clamp01 = (value: number) => Math.min(1, Math.max(0, value));

/**
 * Compact input wire format, version 1. Pointer moves are seven bytes and
 * gamepad reports are four bytes plus fourteen per occupied slot; both are
 * only sent to hosts that advertise `capabilities.binaryInput`.
 */
export function encodeBinaryInput(payload: ControlMessage): ArrayBuffer | null {
  if (payload.type === "pointer" && payload.action === "move") {
    const out = new DataView(new ArrayBuffer(7));
    out.setUint8(0, MAGIC);
    out.setUint8(1, VERSION);
    out.setUint8(2, POINTER_MOVE);
    out.setUint16(3, Math.round(clamp01(payload.x) * 65535), true);
    out.setUint16(5, Math.round(clamp01(payload.y) * 65535), true);
    return out.buffer;
  }
  if (payload.type !== "gamepad" || payload.pads.length > 4) return null;
  const dualShock4 = new Map(payload.dualShock4?.map((pad) => [pad.slot, pad]) ?? []);
  const out = new DataView(new ArrayBuffer(4 + payload.pads.length * 14));
  out.setUint8(0, MAGIC);
  out.setUint8(1, VERSION);
  out.setUint8(2, GAMEPAD);
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
