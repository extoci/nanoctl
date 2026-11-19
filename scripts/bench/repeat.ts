export function repeatArgs(argv: string[]) {
  const args = [...argv];
  const index = args.indexOf("--repeat");
  if (index < 0) return { args, repeat: null };
  const repeat = Number(args[index + 1]);
  args.splice(index, 2);
  if (!Number.isInteger(repeat) || repeat < 1 || repeat > 100 || args.includes("--repeat"))
    throw new Error("--repeat requires an integer from 1 to 100, once");
  return { args, repeat };
}

export type BenchRun = {
  valid: boolean;
  inputMs: { count: number; p50: number | null; p95: number | null };
  videoMs: { p50: number | null; p95: number | null };
  uniqueFps: number;
  resources: { motion: { cpuPercent: number; rssMiB: number } };
};

export function spread(values: number[]): {
  count: number;
  median: number | null;
  min: number | null;
  max: number | null;
} {
  const sorted = values.filter(Number.isFinite).toSorted((a, b) => a - b);
  const middle = Math.floor(sorted.length / 2);
  return {
    count: sorted.length,
    median: sorted.length
      ? (sorted[middle] + sorted[Math.floor((sorted.length - 1) / 2)]) / 2
      : null,
    min: sorted[0] ?? null,
    max: sorted.at(-1) ?? null,
  };
}

export function summarizeRuns(product: string, runs: BenchRun[], requested: number) {
  const valid = runs.filter(
    (run) =>
      run.valid &&
      [
        run.inputMs.p50,
        run.inputMs.p95,
        run.videoMs.p50,
        run.videoMs.p95,
        run.uniqueFps,
        run.resources.motion.cpuPercent,
        run.resources.motion.rssMiB,
      ].every((value) => typeof value === "number" && Number.isFinite(value)),
  );
  const metric = (select: (run: BenchRun) => number | null) =>
    spread(valid.map(select).filter((v): v is number => v !== null));
  return {
    product,
    requested,
    observed: runs.length,
    validRuns: valid.length,
    complete: runs.length === requested && valid.length === requested,
    matchedInputs: valid.reduce((n, run) => n + run.inputMs.count, 0),
    inputP50: metric((run) => run.inputMs.p50),
    inputP95: metric((run) => run.inputMs.p95),
    videoP50: metric((run) => run.videoMs.p50),
    videoP95: metric((run) => run.videoMs.p95),
    fps: metric((run) => run.uniqueFps),
    cpu: metric((run) => run.resources.motion.cpuPercent),
    rss: metric((run) => run.resources.motion.rssMiB),
  };
}

export function summaryMarkdown(summary: ReturnType<typeof summarizeRuns>) {
  const cell = (v: ReturnType<typeof spread>) =>
    v.median === null
      ? "n/a"
      : `${v.median.toFixed(2)} [${v.min!.toFixed(2)}–${v.max!.toFixed(2)}]`;
  return `| Product | Valid/requested | Input p50 ms | Input p95 ms | Video p50 ms | Video p95 ms | Motion CPU % | Motion RSS MiB | FPS |\n| --- | --- | --- | --- | --- | --- | --- | --- | --- |\n| ${summary.product} | ${summary.validRuns}/${summary.requested} | ${cell(summary.inputP50)} | ${cell(summary.inputP95)} | ${cell(summary.videoP50)} | ${cell(summary.videoP95)} | ${cell(summary.cpu)} | ${cell(summary.rss)} | ${cell(summary.fps)} |\n\nCells are median of per-run metrics [min–max], not pooled percentiles. Complete: ${summary.complete}. Matched inputs: ${summary.matchedInputs}. Invalid/missing runs are excluded from metrics but make the comparison incomplete.\n`;
}
