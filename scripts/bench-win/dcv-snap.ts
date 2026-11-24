// Snapshot the viewer's __dcv counters over a window — measures delivered
// fps, wire transit, decode and present latency for the data-channel video
// path. Requires the instrumented viewer (window.__dcv) and Chrome running
// with --remote-debugging-port=9222.
//
// Usage: bun dcv-snap.ts <page-id> [seconds]
// Page ids come from http://localhost:9222/json/list
const page = process.argv[2];
const secs = Number(process.argv[3] ?? 15);
if (!page) {
  console.error("usage: bun dcv-snap.ts <page-id> [seconds]");
  process.exit(2);
}
const ws = new WebSocket(`ws://localhost:9222/devtools/page/${page}`);
let id = 0;
const pending = new Map<number, (v: any) => void>();
const send = (m: string, p: any = {}) =>
  new Promise<any>((r) => {
    const i = ++id;
    pending.set(i, r);
    ws.send(JSON.stringify({ id: i, method: m, params: p }));
  });
ws.onmessage = (e) => {
  const m = JSON.parse(e.data);
  if (m.id && pending.has(m.id)) {
    pending.get(m.id)!(m);
    pending.delete(m.id);
  }
};
await new Promise((r) => (ws.onopen = r));
const evalJs = async (e: string) =>
  (await send("Runtime.evaluate", { expression: e, returnByValue: true })).result?.result?.value;
const snap = () =>
  evalJs("JSON.stringify(window.__dcv ?? {})").then((v) => (v ? JSON.parse(v) : {}));
// settle: let the stream attach before the first snapshot
await new Promise((r) => setTimeout(r, 3000));
const t0: Record<string, number> = await snap();
await new Promise((r) => setTimeout(r, secs * 1000));
const t1: Record<string, number> = await snap();
const delta = Object.fromEntries(Object.keys(t1).map((k) => [k, t1[k]! - (t0[k] ?? 0)]));
const avg = (sum: string, n: string) =>
  +(((t1[sum] ?? 0) - (t0[sum] ?? 0)) / Math.max(1, (t1[n] ?? 0) - (t0[n] ?? 0))).toFixed(2);
console.log(
  JSON.stringify({
    seconds: secs,
    delta,
    transitAvgMs: avg("transitSum", "msgs"),
    decodeAvgMs: avg("decodeSum", "decodeN"),
    presentAvgMs: avg("presentSum", "presentN"),
    decodedPerSec: +((delta.decoded ?? 0) / secs).toFixed(1),
  }),
);
ws.close();
process.exit(0);
