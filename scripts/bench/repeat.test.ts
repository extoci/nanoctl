import { expect, test } from "bun:test";
import { repeatArgs, spread, summarizeRuns, summaryMarkdown, type BenchRun } from "./repeat";

const run = (n: number, valid = true): BenchRun => ({
  valid,
  inputMs: { count: 120, p50: n, p95: n + 10 },
  videoMs: { p50: n - 5, p95: n + 5 },
  uniqueFps: 60,
  resources: { motion: { cpuPercent: n, rssMiB: 55 } },
});

test("repeat option works before or after positional arguments without mutating them", () => {
  const args = ["product", "--repeat", "5", ":97"];
  expect(repeatArgs(args)).toEqual({ args: ["product", ":97"], repeat: 5 });
  expect(args.length).toBe(4);
  expect(repeatArgs(["--repeat", "1", "product"]).repeat).toBe(1);
  expect(repeatArgs(["product"]).repeat).toBeNull();
});
test("rejects missing, repeated, fractional and out-of-range repeat counts", () => {
  for (const args of [
    ["--repeat"],
    ["--repeat", "0"],
    ["--repeat", "1.5"],
    ["--repeat", "101"],
    ["--repeat", "2", "--repeat", "3"],
  ])
    expect(() => repeatArgs(args)).toThrow();
});
test("summarizes per-run quantiles rather than pooling samples, with min-max spread", () => {
  const summary = summarizeRuns("candidate", [run(10), run(40), run(20)], 3);
  expect(summary.inputP50).toEqual({ count: 3, median: 20, min: 10, max: 40 });
  expect(summary.inputP95.median).toBe(30);
  expect(summary.matchedInputs).toBe(360);
  expect(summary.complete).toBe(true);
  expect(summaryMarkdown(summary)).toContain("20.00 [10.00–40.00]");
  expect(spread([4, 2]).median).toBe(3);
});
test("empty, invalid and missing runs never fabricate successful comparisons", () => {
  expect(spread([])).toEqual({ count: 0, median: null, min: null, max: null });
  const summary = summarizeRuns("failed", [run(10), run(100, false)], 3);
  expect(summary.complete).toBe(false);
  expect(summary.observed).toBe(2);
  expect(summary.validRuns).toBe(1);
  expect(summary.inputP50.median).toBe(10);
  const incomplete = run(10);
  incomplete.videoMs.p95 = null;
  expect(summarizeRuns("missing metric", [incomplete], 1).complete).toBe(false);
});
