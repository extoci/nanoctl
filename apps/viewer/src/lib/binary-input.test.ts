import { describe, expect, test } from "bun:test";
import { encodeBinaryInput } from "./binary-input";

describe("encodeBinaryInput", () => {
  test("encodes pointer motion as normalized u16 coordinates", () => {
    const bytes = new Uint8Array(
      encodeBinaryInput({ type: "pointer", action: "move", x: 0.5, y: 1 })!,
    );
    expect([...bytes]).toEqual([0x4e, 1, 1, 0x00, 0x80, 0xff, 0xff]);
  });

  test("encodes version-2 discrete input without JSON framing", () => {
    expect([
      ...new Uint8Array(
        encodeBinaryInput({
          type: "key",
          action: "down",
          code: "KeyA",
          key: "a",
          modifiers: 0,
          repeat: false,
        })!,
      ),
    ]).toEqual([0x4e, 2, 5, 0, 4, 1, 75, 101, 121, 65, 97]);
    expect([
      ...new Uint8Array(
        encodeBinaryInput({
          type: "pointer",
          action: "up",
          x: 0.5,
          y: 1,
          button: 2,
        })!,
      ),
    ]).toEqual([0x4e, 2, 3, 0x82, 0x00, 0x80, 0xff, 0xff]);
    expect([
      ...new Uint8Array(encodeBinaryInput({ type: "release" })!),
    ]).toEqual([0x4e, 2, 7]);
  });

  test("preserves wheel deltas as little-endian float64", () => {
    const bytes = new DataView(
      encodeBinaryInput({
        type: "pointer",
        action: "wheel",
        x: 0,
        y: 0,
        deltaX: -12.5,
        deltaY: 120,
      })!,
    );
    expect(bytes.getFloat64(7, true)).toBe(-12.5);
    expect(bytes.getFloat64(15, true)).toBe(120);
  });

  test("encodes compact gamepad reports including DualShock flags", () => {
    const bytes = new Uint8Array(
      encodeBinaryInput({
        type: "gamepad",
        pads: [[2, 0x1001, 7, 8, -32768, 32767, -1, 1]],
        dualShock4: [
          {
            slot: 2,
            leftTriggerPressed: true,
            rightTriggerPressed: false,
            touchpadPressed: true,
          },
        ],
      })!,
    );
    expect([...bytes]).toEqual([
      0x4e, 1, 2, 1, 2, 0x01, 0x10, 7, 8, 0x00, 0x80, 0xff, 0x7f, 0xff, 0xff,
      0x01, 0x00, 0x0b,
    ]);
  });
});
