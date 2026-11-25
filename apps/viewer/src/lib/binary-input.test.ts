import { describe, expect, test } from "bun:test";
import { encodeBinaryInput } from "./binary-input";

describe("encodeBinaryInput", () => {
  test("encodes pointer motion as normalized u16 coordinates", () => {
    const bytes = new Uint8Array(
      encodeBinaryInput({ type: "pointer", action: "move", x: 0.5, y: 1 })!,
    );
    expect([...bytes]).toEqual([0x4e, 1, 1, 0x00, 0x80, 0xff, 0xff]);
  });

  test("encodes compact gamepad reports including DualShock flags", () => {
    const bytes = new Uint8Array(
      encodeBinaryInput({
        type: "gamepad",
        pads: [[2, 0x1001, 7, 8, -32768, 32767, -1, 1]],
        dualShock4: [
          { slot: 2, leftTriggerPressed: true, rightTriggerPressed: false, touchpadPressed: true },
        ],
      })!,
    );
    expect([...bytes]).toEqual([
      0x4e, 1, 2, 1, 2, 0x01, 0x10, 7, 8, 0x00, 0x80, 0xff, 0x7f, 0xff, 0xff, 0x01, 0x00, 0x0b,
    ]);
  });
});
