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

test("maps standard controls and repeats active snapshots every poll", () => {
  const messages: unknown[] = [];
  const gamepad = new GamepadForwarder((message) => Boolean(messages.push(message)));
  const controller = pad(10);
  gamepad.sample([null, controller, pad(12, "")], 0);
  gamepad.sample([controller], 16);
  const pads = [[0, 5121, 128, 255, -32768, 32767, 16384, -32768]];
  expect(messages).toEqual([
    { type: "gamepad", sequence: 0, pads },
    { type: "gamepad-sparse", sequence: 1, pads },
  ]);
  gamepad.sample([controller], 32);
  gamepad.sample([{ ...controller, axes: [NaN] }], 48);
  gamepad.sample([], 64);
  gamepad.release();
  expect(messages).toEqual([
    { type: "gamepad", sequence: 0, pads },
    { type: "gamepad-sparse", sequence: 1, pads },
    { type: "gamepad-sparse", sequence: 2, pads },
    { type: "gamepad-sparse", sequence: 3, pads: [[0, 5121, 128, 255, 0, 0, 0, 0]] },
    { type: "gamepad", sequence: 4, pads: [] },
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
  const extra = {
    slot: 0,
    leftTriggerPressed: true,
    rightTriggerPressed: false,
  };
  expect(messages).toEqual([
    {
      type: "gamepad",
      sequence: 0,
      pads: reports,
      dualShock4: [{ ...extra, touchpadPressed: false }],
    },
    {
      type: "gamepad-sparse",
      sequence: 1,
      pads: reports,
      dualShock4: [{ ...extra, touchpadPressed: true }],
    },
  ]);
});

test("bounds to four controllers, preserves slots, and retries a blocked release", () => {
  let blocked = false;
  const slots: number[][] = [];
  const gamepad = new GamepadForwarder((message) => {
    if ((message.type !== "gamepad" && message.type !== "gamepad-sparse") || blocked) return false;
    slots.push(message.pads.map((pad) => pad[0] ?? -1));
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
    [0, 1, 2],
  ]);
});

test("repeats a change for 100ms, then heartbeats an unchanged pad about every 250ms", () => {
  const sentAt: number[] = [];
  const gamepad = new GamepadForwarder(() => Boolean(sentAt.push(now)));
  const controller = pad(0);
  let now = 0;
  for (; now <= 400; now += 20) gamepad.sample([controller], now);
  expect(sentAt).toEqual([0, 20, 40, 60, 80, 340]);
  const pressed = { ...controller, axes: [0, 0, 0, 0] };
  for (; now <= 800; now += 20) gamepad.sample([pressed], now);
  expect(sentAt.slice(6)).toEqual([420, 440, 460, 480, 500, 760]);
});
