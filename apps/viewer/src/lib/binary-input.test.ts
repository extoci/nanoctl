import { describe, expect, test } from "bun:test";
import { encodeBinaryInput, encodeGamepadHeartbeat } from "./binary-input";

describe("encodeBinaryInput", () => {
  test("encodes pointer motion as normalized u16 coordinates", () => {
    const bytes = new Uint8Array(
      encodeBinaryInput({ type: "pointer", action: "move", x: 0.5, y: 1 })!,
    );
    expect([...bytes]).toEqual([1, 0x00, 0x80, 0xff, 0xff]);
  });

  test("encodes relative pointer motion as pixel deltas", () => {
    const bytes = new Uint8Array(
      encodeBinaryInput({ type: "pointer", action: "move", x: 0, y: 0, deltaX: -12, deltaY: 300 })!,
    );
    expect([...bytes]).toEqual([14, 0xf4, 0xff, 0x2c, 0x01]);
  });

  test("encodes keys as extended physical-code records", () => {
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
    ]).toEqual([13, 0, 4, 1, 75, 101, 121, 65, 97]);
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
    ]).toEqual([13, 3, 9, 5, 83, 104, 105, 102, 116, 76, 101, 102, 116, 83, 104, 105, 102, 116]);
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
    const report = {
      sequence: 9,
      pads: [[2, 0x1001, 7, 8, -32768, 32767, -1, 1]] as const,
      dualShock4: [
        {
          slot: 2,
          leftTriggerPressed: true,
          rightTriggerPressed: false,
          touchpadPressed: true,
        },
      ],
    };
    const bytes = new Uint8Array(encodeBinaryInput({ type: "gamepad", ...report })!);
    expect([...bytes]).toEqual([
      2, 9, 4, 0x01, 0x10, 7, 8, 0x00, 0x80, 0xff, 0x7f, 0xff, 0xff, 0x01, 0x00, 0x0b,
    ]);
    const sparse = new Uint8Array(encodeBinaryInput({ type: "gamepad-sparse", ...report })!);
    expect([...sparse]).toEqual([
      12, 9, 4, 0xff, 0x81, 0x20, 7, 8, 0x00, 0x80, 0xff, 0x7f, 0xff, 0xff, 0x01, 0x00, 0x0b,
    ]);
  });

  test("omits zero fields from sparse gamepad reports", () => {
    const sparse = new Uint8Array(
      encodeBinaryInput({
        type: "gamepad-sparse",
        sequence: 10,
        pads: [[1, 0, 0, 7, -2, 0, 0, 0]] as const,
      })!,
    );
    expect([...sparse]).toEqual([12, 10, 2, 12, 7, 0xfe, 0xff]);
  });
});
