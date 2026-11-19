import { expect, test } from "bun:test";
import type { ControlMessage } from "@nanoctl/protocol";
import { TouchGestures, normalizedPosition, videoGeometry, type TouchMode } from "./touch-input";

const sample = (id: number, x = 100, y = 100, t = 0) => ({ id, x, y, t });
function setup(mode: TouchMode = "trackpad") {
  const messages: ControlMessage[] = [];
  const zooms: unknown[] = [];
  let timer: (() => void) | undefined;
  const gestures = new TouchGestures({
    mode,
    geometry: () => ({ left: 0, top: 0, width: 1000, height: 500 }),
    send: (message) => messages.push(message),
    onZoom: (zoom) => zooms.push(zoom),
    clock: {
      setTimeout: (fn) => {
        timer = fn;
        return 1;
      },
      clearTimeout: () => {
        timer = undefined;
      },
    },
  });
  return { gestures, messages, zooms, fire: () => timer?.() };
}
const buttons = (messages: ControlMessage[]) =>
  messages.filter((m) => m.type === "pointer" && m.action !== "move");

test("trackpad tap clicks at centered cursor; direct tap moves to finger", () => {
  for (const mode of ["trackpad", "direct"] as const) {
    const { gestures: g, messages } = setup(mode);
    g.down(sample(1));
    g.up(sample(1, 100, 100, 100));
    expect(buttons(messages)).toEqual([
      {
        type: "pointer",
        action: "down",
        x: mode === "direct" ? 0.1 : 0.5,
        y: mode === "direct" ? 0.2 : 0.5,
        button: 0,
      },
      {
        type: "pointer",
        action: "up",
        x: mode === "direct" ? 0.1 : 0.5,
        y: mode === "direct" ? 0.2 : 0.5,
        button: 0,
      },
    ]);
  }
});
test("trackpad drag moves relatively without pressing; direct drag releases", () => {
  for (const mode of ["trackpad", "direct"] as const) {
    const { gestures: g, messages } = setup(mode);
    g.down(sample(1));
    g.move(sample(1, 200, 100, 100));
    g.up(sample(1, 200, 100, 300));
    expect(g.cursor.x).toBeGreaterThan(mode === "trackpad" ? 0.5 : 0.1);
    expect(buttons(messages).map((m) => m.type === "pointer" && m.action)).toEqual(
      mode === "direct" ? ["down", "up"] : [],
    );
  }
});
test("double tap and hold drags and cancel releases", () => {
  const { gestures: g, messages } = setup();
  g.down(sample(1));
  g.up(sample(1, 100, 100, 100));
  g.down(sample(1, 100, 100, 200));
  g.move(sample(1, 250, 100, 400));
  g.cancel();
  expect(buttons(messages).map((m) => m.type === "pointer" && m.action)).toEqual([
    "down",
    "up",
    "down",
    "up",
  ]);
  const count = messages.length;
  g.reset();
  expect(messages.length).toBe(count);
});
test("two and three finger taps click right and middle", () => {
  for (const count of [2, 3]) {
    const { gestures: g, messages } = setup();
    for (let id = 1; id <= count; id++) g.down(sample(id, id * 100));
    for (let id = 1; id <= count; id++) g.up(sample(id, id * 100, 100, 100));
    expect(buttons(messages).map((m) => m.type === "pointer" && m.button)).toEqual([
      count === 2 ? 2 : 1,
      count === 2 ? 2 : 1,
    ]);
  }
});
test("parallel two finger motion emits whole natural scroll steps", () => {
  const { gestures: g, messages } = setup();
  g.down(sample(1));
  g.down(sample(2, 200));
  g.move(sample(1, 100, 140, 100));
  g.move(sample(2, 200, 140, 100));
  g.up(sample(1, 100, 140, 200));
  g.up(sample(2, 200, 140, 200));
  expect(
    messages.some((m) => m.type === "pointer" && m.action === "wheel" && m.deltaY === -100),
  ).toBe(true);
  expect(messages.every((m) => m.type === "pointer" && m.action === "wheel")).toBe(true);
});
test("direct long press right clicks without a subsequent left click", () => {
  const { gestures: g, messages, fire } = setup("direct");
  g.down(sample(1));
  fire();
  g.up(sample(1, 100, 100, 600));
  expect(buttons(messages).map((m) => m.type === "pointer" && m.button)).toEqual([2, 2]);
});
test("pinch zooms and pans locally, pinching back resets", () => {
  const { gestures: g, messages, zooms } = setup();
  g.down(sample(1));
  g.down(sample(2, 200));
  g.move(sample(1, 80, 100, 100));
  expect(g.zoom.scale).toBeCloseTo(1.2);
  g.move(sample(2, 230, 110, 110));
  expect(g.zoom.offsetY).toBe(5);
  g.move(sample(1, 130, 110, 120));
  expect(g.zoom.scale).toBe(1);
  g.up(sample(1, 130, 110, 130));
  g.up(sample(2, 230, 110, 130));
  expect(zooms.length).toBeGreaterThan(0);
  expect(messages).toEqual([]);
});
test("double two finger tap resets zoom", () => {
  const { gestures: g } = setup();
  g.zoom = { scale: 2, offsetX: 20, offsetY: 30 };
  for (const t of [0, 200]) {
    g.down(sample(1, 100, 100, t));
    g.down(sample(2, 200, 100, t));
    g.up(sample(1, 100, 100, t + 50));
    g.up(sample(2, 200, 100, t + 50));
  }
  expect(g.zoom).toEqual({ scale: 1, offsetX: 0, offsetY: 0 });
});
test("empty and unknown input is harmless and missing geometry does not move", () => {
  const messages: ControlMessage[] = [];
  const g = new TouchGestures({
    mode: "direct",
    geometry: () => null,
    send: (m) => messages.push(m),
  });
  g.up(sample(1));
  g.move(sample(1));
  g.cancel();
  g.reset();
  expect(messages).toEqual([]);
  expect(videoGeometry({ left: 0, top: 0, width: 0, height: 0 }, 100, 100)).toBeNull();
});
test("letterboxed coordinates use transformed rect for zoom and pan", () => {
  const rect = { left: 10, top: 20, width: 400, height: 400 };
  const normal = videoGeometry(rect, 200, 100)!;
  expect(normal).toEqual({ left: 10, top: 120, width: 400, height: 200 });
  expect(normalizedPosition({ x: 110, y: 170 }, normal)).toEqual({ x: 0.25, y: 0.25 });
  const zoomed = videoGeometry({ left: -160, top: -140, width: 800, height: 800 }, 200, 100)!;
  expect(normalizedPosition({ x: 40, y: 160 }, zoomed)).toEqual({ x: 0.25, y: 0.25 });
  expect(normalizedPosition({ x: -1000, y: 2000 }, zoomed)).toEqual({ x: 0, y: 1 });
});
