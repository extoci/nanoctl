import { expect, test } from "bun:test";
import { trialPlan } from "./phases";

test("quality keeps its isolated twenty-second motion phase by default", () => {
  expect(trialPlan(true, false, false)).toEqual({ trials: 0, duration: 20 });
  expect(trialPlan(true, false, false, "invalid")).toEqual({ trials: 0, duration: 20 });
});

test("quality input trials and ordinary motion allow all inputs to finish", () => {
  expect(trialPlan(true, false, true)).toEqual({ trials: 120, duration: 59 });
  expect(trialPlan(false, false, false, "100")).toEqual({ trials: 100, duration: 50 });
  expect(trialPlan(true, false, true, "250")).toEqual({ trials: 250, duration: 118 });
  expect(trialPlan(false, true, false)).toEqual({ trials: 120, duration: 107 });
});

test("quality input trials reject empty and invalid counts", () => {
  for (const count of ["", "0", "99", "251", "120.5", "invalid", "Infinity"]) {
    expect(() => trialPlan(true, false, true, count)).toThrow("Invalid trials");
    expect(() => trialPlan(false, false, false, count)).toThrow("Invalid trials");
    expect(() => trialPlan(false, true, false, count)).toThrow("Invalid trials");
  }
});

test("incompatible phase switches fail before launching a stack", () => {
  expect(() => trialPlan(true, true, false)).toThrow("separate trials");
  expect(() => trialPlan(false, false, true)).toThrow("require BENCH_QUALITY");
});
