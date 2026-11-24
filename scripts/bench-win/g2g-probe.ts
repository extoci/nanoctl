// In-page glass-to-glass probe: reads the clock marker painted by
// `benchwin band --clock` out of the streamed <video> element and diffs
// it against the local wall clock. Run Chrome with
// --remote-debugging-port=9222 pointed at the viewer page.
//
// Usage: bun g2g-probe.ts <page-id> [seconds]
// Page ids come from http://localhost:9222/json/list
const page = process.argv[2];
const secs = Number(process.argv[3] ?? 30);
if (!page) {
  console.error("usage: bun g2g-probe.ts <page-id> [seconds]");
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
await send("Runtime.enable");

const probe = `
(() => {
  const v = document.querySelector('video');
  if (!v || !v.videoWidth) return 'novideo';
  if (!window.__g2g) {
    const c = document.createElement('canvas');
    const ctx = c.getContext('2d', { willReadFrequently: true });
    window.__g2g = { c, ctx, samples: [] };
  }
  const g = window.__g2g;
  g.c.width = v.videoWidth; g.c.height = v.videoHeight;
  g.ctx.drawImage(v, 0, 0);
  // marker band is the top strip of the remote desktop (rows ~4..44 of 1200)
  for (let y = 2; y < Math.min(60, v.videoHeight); y += 2) {
    const px = g.ctx.getImageData(0, y, v.videoWidth, 1).data;
    let left = -1, right = -1;
    for (let x = 0; x < v.videoWidth; x++) {
      const r = px[x*4], gg = px[x*4+1], b = px[x*4+2];
      if (r < 80 && gg > 150 && b > 150) left = x;
      if (left >= 0 && r > 150 && gg < 80 && b > 150) { right = x; break; }
    }
    if (left < 0 || right - left < 96) continue;
    const bits = [];
    for (let i = 0; i < 36; i++) {
      const x = Math.floor(left + (right - left) * (2*i+1) / 72);
      const red = px[x*4];
      bits.push(red > 192 ? 1 : red < 64 ? 0 : -1);
    }
    if (bits[0]!==1 || bits[1]!==0 || bits[34]!==0 || bits[35]!==1) continue;
    let word = 0, bad = false;
    for (const b of bits.slice(2,34)) { if (b<0){bad=true;break} word = (word<<1)|b; }
    if (bad) continue;
    const seq = word>>>16, input=(word>>>8)&0xff;
    if ((word&0xff) !== ((((seq>>8)^seq)&0xff) ^ input ^ 0xa5)) continue;
    const now = Date.now() % 60000;
    let lag = now - seq; if (lag < -30000) lag += 60000; if (lag > 30000) lag -= 60000;
    g.samples.push({t: Date.now(), lag});
    return 'ok';
  }
  return 'nomarker';
})()`;

const t0 = Date.now();
while (Date.now() - t0 < secs * 1000) {
  await send("Runtime.evaluate", { expression: probe, returnByValue: true });
  await new Promise((r) => setTimeout(r, 50));
}
const dump = await send("Runtime.evaluate", {
  expression: "JSON.stringify(window.__g2g?.samples||[])",
  returnByValue: true,
});
const samples = JSON.parse(dump.result.result.value || "[]");
const ls = samples
  .map((s: { lag: number }) => s.lag)
  .filter((l: number) => l > 0)
  .sort((a: number, b: number) => a - b);
const n = ls.length;
if (!n) {
  console.log("no samples");
  ws.close();
  process.exit(1);
}
const stats = {
  n,
  min: ls[0],
  p50: ls[Math.floor(n * 0.5)],
  p90: ls[Math.floor(n * 0.9)],
  p99: ls[Math.floor(n * 0.99)],
  max: ls[n - 1],
  mean: +(ls.reduce((a: number, b: number) => a + b, 0) / n).toFixed(1),
};
console.log(JSON.stringify(stats));
ws.close();
