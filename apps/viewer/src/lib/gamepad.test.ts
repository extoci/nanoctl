import { expect, test } from "bun:test";
import { GamepadForwarder } from "./gamepad";

function pad(index: number, mapping: GamepadMappingType = "standard") {
  return {
    axes: [-1, -1, 0.5, 1],
    buttons: Array.from({ length: 17 }, (_, button) => ({
      pressed: [0, 6, 12, 16].includes(button),
      touched: false,
      value: button === 6 ? 0.5 : button === 7 ? 1 : 0,
    })),
    connected: true,
    id: "Xbox Wireless Controller",
    index,
    mapping,
  };
}

test("reports whether the browser sees a supported controller", () => {
  const gamepad = new GamepadForwarder(() => true);
  expect(gamepad.sample([], 0)).toBe("waiting");
  expect(gamepad.sample([pad(1, "")], 1)).toBe("unsupported");
  expect(gamepad.sample([pad(1, ""), pad(2)], 2)).toBe("connected");
});

test("maps standard controls and sends snapshots on change or keepalive", () => {
  const messages: unknown[] = [];
  const gamepad = new GamepadForwarder((message) => Boolean(messages.push(message)));
  const controller = pad(10);
  gamepad.sample([null, controller, pad(12, "")], 0);
  gamepad.sample([controller], 499);
  const state = { type: "gamepad", pads: [[0, 5121, 128, 255, -32768, 32767, 16384, -32768]] };
  expect(messages).toEqual([state]);
  gamepad.sample([controller], 500);
  gamepad.sample([{ ...controller, axes: [NaN] }], 501);
  gamepad.sample([], 502);
  gamepad.release();
  expect(messages).toEqual([
    state,
    state,
    { type: "gamepad", pads: [[0, 5121, 128, 255, 0, 0, 0, 0]] },
    { type: "gamepad", pads: [] },
  ]);
});

test.each([
  "DualSense Wireless Controller Extended Gamepad",
  "DualShock 4 Wireless Controller",
  "DUALSHOCK4",
  "Wireless Controller",
  "Wireless Controller Extended Gamepad",
  "054c device (STANDARD GAMEPAD Vendor: 054c Product: 0ce6)",
  "PlayStation Controller",
])("identifies %s as a DualShock 4 target", (id) => {
  const messages: unknown[] = [];
  const gamepad = new GamepadForwarder((message) => Boolean(messages.push(message)));
  const controller = { ...pad(10), id };
  gamepad.sample([controller], 0);
  gamepad.sample(
    [
      {
        ...controller,
        buttons: [...controller.buttons, { pressed: true, touched: false, value: 1 }],
      },
    ],
    1,
  );
  const reports = [[0, 5121, 128, 255, -32768, 32767, 16384, -32768]];
  const extra = { slot: 0, leftTriggerPressed: true, rightTriggerPressed: false };
  expect(messages).toEqual([
    { type: "gamepad", pads: reports, dualShock4: [{ ...extra, touchpadPressed: false }] },
    { type: "gamepad", pads: reports, dualShock4: [{ ...extra, touchpadPressed: true }] },
  ]);
});

test("bounds to four controllers, preserves slots, and retries a blocked release", () => {
  let blocked = false;
  const slots: number[][] = [];
  const gamepad = new GamepadForwarder(({ pads }) => {
    if (blocked) return false;
    slots.push(pads.map((pad) => pad[0]));
    return true;
  });
  gamepad.sample([pad(2), pad(3), pad(4), pad(8), pad(9)], 0);
  // Renumbered controllers keep their slots when the pad id is still present.
  gamepad.sample([pad(8), pad(3), pad(9)], 1);
  gamepad.sample([pad(8), pad(3), pad(9)], 2);
  blocked = true;
  gamepad.release();
  blocked = false;
  gamepad.release();
  expect(slots).toEqual([
    [0, 1, 2, 3],
    [0, 1, 2],
    [0, 1, 2],
  ]);
});
