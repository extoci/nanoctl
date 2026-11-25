import { describe, expect, test } from "bun:test";
import { encodeBinaryInput, encodeGamepadHeartbeat } from "./binary-input";

describe("encodeBinaryInput", () => {
  test("encodes pointer motion as normalized u16 coordinates", () => {
    const bytes = new Uint8Array(
      encodeBinaryInput({ type: "pointer", action: "move", x: 0.5, y: 1 })!,
    );
    expect([...bytes]).toEqual([1, 0x00, 0x80, 0xff, 0xff]);
  });

  test("encodes keys as named ids or UTF-8 without code strings", () => {
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
    ]).toEqual([5, 0, 97]);
    expect([
      ...new Uint8Array(
        encodeBinaryInput({
          type: "key",
          action: "up",
          code: "ShiftLeft",
          key: "Shift",
          modifiers: 0,
          repeat: true,
        })!,
      ),
    ]).toEqual([5, 7, 30]);
  });

  test("encodes discrete input without legacy framing", () => {
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
    ]).toEqual([3, 0x82, 0x00, 0x80, 0xff, 0xff]);
    expect([...new Uint8Array(encodeBinaryInput({ type: "release" })!)]).toEqual([7]);
    expect([...new Uint8Array(encodeGamepadHeartbeat())]).toEqual([11]);
    expect([
      ...new Uint8Array(encodeBinaryInput({ type: "ping", nonce: 9, sentAt: 123 })!),
    ]).toEqual([8]);
  });

  test("preserves exact wheel deltas as trailing-zero-truncated float64", () => {
    const bytes = new Uint8Array(
      encodeBinaryInput({
        type: "pointer",
        action: "wheel",
        x: 0,
        y: 0,
        deltaX: -12.5,
        deltaY: 120,
      })!,
    );
    expect([...bytes]).toEqual([4, 0, 0, 0, 0, 0x22, 0xc0, 0x29, 0x40, 0x5e]);
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
      2, 1, 2, 0x01, 0x10, 7, 8, 0x00, 0x80, 0xff, 0x7f, 0xff, 0xff, 0x01, 0x00, 0x0b,
    ]);
  });
});
