import { readFileSync, writeFileSync } from "node:fs";
import { stripVTControlCharacters } from "node:util";
import { distribution } from "./metrics";

type Key = { at: number; code: string; repeat: boolean; trusted: boolean };
type ViewerFrame = {
  sequence: number;
  input: number;
  at: number;
  receiveTime?: number;
  presentationTime: number;
  expectedDisplayTime: number;
  processingDuration?: number;
};
type Result = {
  source: { offsetMs: number; uncertaintyMs: number };
  raw: {
    paints: number[];
    viewerKeys: Key[];
    attribution: {
      clock: { offsetMs: number; uncertaintyMs: number };
      frames: ViewerFrame[];
      sends: { at: number }[];
    };
  };
};
type Source = { keys: Key[]; inputPaints: { input: number; at: number }[] };

export function attribute(result: Result, source: Source, pixelLog: string, agentLog: string) {
  const native = new Map<string, Map<number, number[]>>();
  for (const line of stripVTControlCharacters(agentLog).split("\n")) {
    const m = /stage="([\w_]+)" sequence=(\d+) mono_ms=([\d.]+)/.exec(line);
    if (!m) continue;
    const stage = native.get(m[1]) ?? new Map<number, number[]>();
    const times = stage.get(+m[2]) ?? [];
    times.push(+m[3]);
    stage.set(+m[2], times);
    native.set(m[1], stage);
  }
  const firstPixel = new Map<number, number>();
  const injections = new Map<number, number>();
  for (const line of pixelLog.split("\n")) {
    const [kind, time, sequence] = line.split(" ");
    if (kind === "S" && +sequence >= 0 && !firstPixel.has(+sequence))
      firstPixel.set(+sequence, +time);
    if (kind === "I") injections.set(+sequence, +time);
  }
  const segments = new Map<string, number[]>();
  const add = (name: string, from: number | undefined, to: number | undefined) => {
    if (from === undefined || to === undefined || !Number.isFinite(from) || !Number.isFinite(to))
      return;
    const times = segments.get(name) ?? [];
    times.push(to - from);
    segments.set(name, times);
  };
  const earliest = (stage: string, seq: number) => native.get(stage)?.get(seq)?.[0];
  const before = (stage: string, seq: number, end: number | undefined) =>
    native
      .get(stage)
      ?.get(seq)
      ?.findLast((t) => end !== undefined && t <= end);
  const offset = result.raw.attribution.clock.offsetMs;
  const local = (t: number | undefined) => (t === undefined ? undefined : t + offset);
  const seen = new Set<number>();
  for (const frame of result.raw.attribution.frames) {
    const seq = frame.sequence;
    if (!seq || seen.has(seq) || !firstPixel.has(seq)) continue;
    seen.add(seq);
    const draw =
      result.raw.paints[seq] === undefined
        ? undefined
        : result.raw.paints[seq] + result.source.offsetMs;
    const encodeStart = earliest("encode_start", seq);
    const captureEnd = before("capture_end", seq, encodeStart);
    const captureStart = before("capture_start", seq, captureEnd);
    const damage = before("damage", seq, captureStart);
    const encodeEnd = earliest("encode_end", seq);
    const sendStart = earliest("send_start", seq);
    const sendEnd = earliest("send_end", seq);
    add("draw → damage received", draw, damage);
    add("damage → capture start", damage, captureStart);
    add("capture start → end", captureStart, captureEnd);
    add("capture end → encode start", captureEnd, encodeStart);
    add("encode start → end", encodeStart, encodeEnd);
    add("encode end → RTP write start", encodeEnd, sendStart);
    add("RTP write start → end", sendStart, sendEnd);
    add("RTP write end → receiveTime", sendEnd, local(frame.receiveTime));
    add("receiveTime → presentationTime", local(frame.receiveTime), local(frame.presentationTime));
    if (frame.processingDuration !== undefined)
      add("decode processingDuration", 0, frame.processingDuration * 1000);
    add(
      "presentationTime → expectedDisplayTime",
      frame.presentationTime,
      frame.expectedDisplayTime,
    );
    add("expectedDisplayTime → first pixel", local(frame.expectedDisplayTime), firstPixel.get(seq));
    add("presentationTime → first pixel", local(frame.presentationTime), firstPixel.get(seq));
    add("draw → first pixel", draw, firstPixel.get(seq));
  }
  const viewerKeys = result.raw.viewerKeys.filter(
    (k) => k.code === "Space" && k.trusted && !k.repeat,
  );
  const hostKeys = source.keys.filter((k) => k.code === "Space" && k.trusted && !k.repeat);
  for (const [input, injected] of injections) {
    const key = local(viewerKeys[input - 1]?.at);
    const send = local(result.raw.attribution.sends[input - 1]?.at);
    const received = earliest("input_receive", input);
    const start = earliest("inject_start", input);
    const end = earliest("inject_end", input);
    const hostKey = hostKeys[input - 1]?.at;
    const repaint = source.inputPaints.find((p) => p.input === input)?.at;
    add("XTest → viewer DOM", injected, key);
    add("viewer DOM → datachannel send", key, send);
    add("datachannel send → agent receive", send, received);
    add("agent receive → inject start", received, start);
    add("inject start → end", start, end);
    add(
      "inject end → source DOM",
      end,
      hostKey === undefined ? undefined : hostKey + result.source.offsetMs,
    );
    add("source DOM → repaint", hostKey, repaint);
    add(
      "XTest → source repaint",
      injected,
      repaint === undefined ? undefined : repaint + result.source.offsetMs,
    );
  }
  const rows = [...segments].map(([segment, samples]) => ({
    segment,
    ...distribution(samples),
    negative: samples.filter((x) => x < 0).length,
  }));
  return {
    clocks: { source: result.source, viewer: result.raw.attribution.clock },
    matchedPixelFrames: seen.size,
    rows,
  };
}

if (import.meta.main) {
  const [prefix, agentLog] = process.argv.slice(2);
  if (!prefix || !agentLog)
    throw new Error("Usage: bun scripts/bench/attribution.ts RESULT_PREFIX AGENT_LOG");
  const report = attribute(
    JSON.parse(readFileSync(`${prefix}.json`, "utf8")),
    JSON.parse(readFileSync(`${prefix}.source.json`, "utf8")),
    readFileSync(`${prefix}.pixels.log`, "utf8"),
    readFileSync(agentLog, "utf8"),
  );
  const markdown =
    "| Segment | N | p50 ms | p95 ms | Negative deltas |\n| --- | ---: | ---: | ---: | ---: |\n" +
    report.rows
      .map(
        (r) =>
          `| ${r.segment} | ${r.count} | ${r.p50?.toFixed(3)} | ${r.p95?.toFixed(3)} | ${r.negative} |`,
      )
      .join("\n") +
    "\n";
  writeFileSync(`${prefix}.attribution.json`, JSON.stringify(report, null, 2));
  writeFileSync(`${prefix}.attribution.md`, markdown);
  console.log(markdown);
}
