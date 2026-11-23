import { expect, test } from "bun:test";
import { sourceContextOptions } from "./source";

test("stimuli default to an opaque low-latency canvas", () => {
  expect(sourceContextOptions()).toEqual({
    alpha: false,
    desynchronized: true,
  });
  expect(sourceContextOptions("low-latency")).toEqual(sourceContextOptions());
});

test("legacy stimuli preserve the original context options", () => {
  expect(sourceContextOptions("legacy")).toEqual({});
});

test("empty and unknown source renderer names are rejected", () => {
  for (const mode of ["", "1", "synchronous", "unknown"])
    expect(() => sourceContextOptions(mode)).toThrow("Invalid source renderer");
});
