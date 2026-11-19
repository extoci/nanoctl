import { expect, test } from "bun:test";
import { drawWorkload, workloadName } from "./workloads";
test("workloads default to the legacy marker and reject unknown names", () => {
  expect(workloadName()).toBe("marker");
  for (const name of ["text", "drag", "gradient"] as const) expect(workloadName(name)).toBe(name);
  for (const name of ["", "video", "unknown"]) expect(() => workloadName(name)).toThrow();
});
test("realistic workloads are deterministic and change with frame ID", () => {
  const draw = (name: string, frame: number) => {
    const calls: unknown[] = [];
    const ctx = new Proxy(
      {},
      {
        set: (_t, key, value) => {
          calls.push([key, value]);
          return true;
        },
        get:
          (_t, key) =>
          (...args: unknown[]) => {
            calls.push([key, args]);
            return { addColorStop: (...v: unknown[]) => calls.push(v) };
          },
      },
    );
    drawWorkload(ctx as CanvasRenderingContext2D, frame, name);
    return JSON.parse(JSON.stringify(calls));
  };
  expect(draw("marker", 1)).toEqual([]);
  for (const name of ["text", "drag", "gradient"]) {
    expect(draw(name, 10)).toEqual(draw(name, 10));
    expect(draw(name, 10)).not.toEqual(draw(name, 11));
  }
});
