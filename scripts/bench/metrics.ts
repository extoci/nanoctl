export function distribution(values: number[]) {
  if (!values.length)
    return { count: 0, mean: null, min: null, p50: null, p95: null, p99: null, max: null };
  const sorted = [...values].sort((a, b) => a - b);
  const rank = (p: number) => sorted[Math.ceil(sorted.length * p) - 1];
  return {
    count: values.length,
    mean: values.reduce((a, b) => a + b, 0) / values.length,
    min: sorted[0],
    p50: rank(0.5),
    p95: rank(0.95),
    p99: rank(0.99),
    max: sorted.at(-1)!,
  };
}

export function analyze(raw: string, paints: number[], sourceOffset: number) {
  const samples: { time: number; sequence: number; input: number }[] = [];
  const inputs: { time: number; input: number }[] = [];
  const readMs: number[] = [],
    workMs: number[] = [];
  let cpuMs = 0,
    rssKiB = 0;
  for (const line of raw.trim().split("\n")) {
    const [kind, ...numbers] = line.split(" ");
    const [time, sequence, input, read, work] = numbers.map(Number);
    if (kind === "I") inputs.push({ time, input: sequence });
    if (kind === "S") {
      samples.push({ time, sequence, input });
      readMs.push(read);
      workMs.push(work);
    }
    if (kind === "Q") {
      cpuMs = time;
      rssKiB = sequence;
    }
  }
  if (samples.length < 2) throw new Error("No detector samples");
  const valid = samples.filter((s) => s.sequence >= 0);
  const warm = samples[0].time + 1000;
  const unique = valid.filter(
    (s, i) => s.time >= warm && (!i || s.sequence !== valid[i - 1].sequence),
  );
  const gaps = unique.slice(1).map((s, i) => s.time - unique[i].time);
  const polling = samples.slice(1).map((s, i) => s.time - samples[i].time);
  const inputMs: number[] = [],
    missingInputs: number[] = [];
  for (const press of inputs) {
    const response = valid.find((s) => s.time >= press.time && s.input === press.input);
    if (response) inputMs.push(response.time - press.time);
    else missingInputs.push(press.input);
  }
  const videoMs = unique.flatMap((s) =>
    paints[s.sequence] === undefined ? [] : [s.time - (paints[s.sequence] + sourceOffset)],
  );
  const elapsed = samples.at(-1)!.time - samples[0].time;
  return {
    inputMs: distribution(inputMs),
    missingInputs,
    injected: inputs.length,
    videoMs: distribution(videoMs),
    uniqueFrames: unique.length,
    uniqueFps:
      unique.length > 1 ? ((unique.length - 1) * 1000) / (unique.at(-1)!.time - unique[0].time) : 0,
    interFrameMs: distribution(gaps),
    stallsOver100Ms: gaps.filter((v) => v > 100).length,
    detector: {
      samples: samples.length,
      validSamples: valid.length,
      hz: ((samples.length - 1) * 1000) / elapsed,
      pollingMs: distribution(polling),
      readMs: distribution(readMs),
      workMs: distribution(workMs),
      cpuPercent: (cpuMs / elapsed) * 100,
      maxRssKiB: rssKiB,
    },
  };
}
