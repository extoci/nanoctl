// Input→photon: click into the streamed video via CDP Input.dispatchMouseEvent,
// watch the band marker's INPUT_COUNT byte bump inside the decoded stream.
// Loopback rig: agent on :7338, Chrome on :9222, `benchwin band --clock --height 60`.
// Usage: bun input-photon.ts <page-id> [clicks]
const page = process.argv[2];
const clicks = Number(process.argv[3] ?? 10);
const ws = new WebSocket(`ws://localhost:9222/devtools/page/${page}`);
let id = 0; const pending = new Map();
const send = (m, p = {}) => new Promise(r => { const i = ++id; pending.set(i, r); ws.send(JSON.stringify({ id: i, method: m, params: p })); });
ws.onmessage = e => { const m = JSON.parse(e.data); if (m.id && pending.has(m.id)) { pending.get(m.id)!(m); pending.delete(m.id); } };
await new Promise(r => ws.onopen = r);
const ev = async (e) => (await send("Runtime.evaluate", { expression: e, returnByValue: true })).result?.result?.value;
const sleep = (ms) => new Promise(r => setTimeout(r, ms));
await send("Runtime.enable");

const decodeFn = `
window.__dec = (v) => {
  for (let y = 2; y < Math.min(56, v.videoHeight); y += 2) {
    const px = window.__inp.ctx.getImageData(0, y, v.videoWidth, 1).data;
    let left = -1, right = -1;
    for (let x = 0; x < v.videoWidth; x++) {
      const r = px[x*4], g = px[x*4+1], b = px[x*4+2];
      if (r < 80 && g > 150 && b > 150) left = x;
      if (left >= 0 && r > 150 && g < 80 && b > 150) { right = x; break; }
    }
    if (left < 0 || right - left < 96) continue;
    const bits = [];
    for (let i = 0; i < 36; i++) {
      const x = Math.floor(left + (right - left) * (2*i+1) / 72);
      const red = px[x*4];
      bits.push(red > 192 ? 1 : red < 64 ? 0 : -1);
    }
    if (bits[0] !== 1 || bits[1] !== 0 || bits[34] !== 0 || bits[35] !== 1) continue;
    let word = 0;
    for (let i = 2; i < 34; i++) { if (bits[i] < 0) { word = -1; break; } word = (word << 1) | bits[i]; }
    if (word < 0) continue;
    const seq = (word >>> 16) & 0xffff, inp = (word >>> 8) & 0xff;
    const chk = word & 0xff;
    if (chk !== ((((seq >> 8) ^ seq) & 0xff) ^ inp ^ 0xa5)) continue;
    return [seq, inp];
  }
  return null;
};`;

const setup = `
(() => {
  const v = document.querySelector('video');
  if (!v || !v.videoWidth) return 'novideo';
  if (!window.__inp) {
    const c = document.createElement('canvas');
    window.__inp = { c, ctx: c.getContext('2d', { willReadFrequently: true }) };
  }
  ${decodeFn}
  window.__inp.c.width = v.videoWidth; window.__inp.c.height = v.videoHeight;
  window.__inp.samples = [];
  const poll = () => {
    const v = document.querySelector('video');
    if (!v || !v.videoWidth) return;
    window.__inp.ctx.drawImage(v, 0, 0);
    const t = performance.now();
    const d = window.__dec(v);
    if (d) window.__inp.samples.push([t, d[0], d[1]]);
  };
  if (window.__inp.timer) clearInterval(window.__inp.timer);
  window.__inp.timer = setInterval(poll, 8);
  const r = v.getBoundingClientRect();
  return JSON.stringify({ x: r.x, y: r.y, w: r.width, h: r.height, vw: v.videoWidth, vh: v.videoHeight });
})()`;

const rectRaw = await ev(setup);
if (rectRaw === 'novideo' || !rectRaw) { console.log('novideo'); ws.close(); process.exit(1); }
const rect = JSON.parse(rectRaw);
const scale = Math.min(rect.w / rect.vw, rect.h / rect.h ? rect.h / rect.vh : 1);
const rw = rect.vw * scale, rh = rect.vh * scale;
const ox = rect.x + (rect.w - rw) / 2, oy = rect.y + (rect.h - rh) / 2;
console.log('video', JSON.stringify(rect), 'rendered', Math.round(rw), Math.round(rh));

await sleep(3000);
const results = [];
const G2G = [];
for (let i = 0; i < clicks; i++) {
  // pick a random x within the band, remote y ~ 24/1200 down
  const fx = 0.15 + Math.random() * 0.7;
  const cx = ox + rw * fx, cy = oy + rh * (24 / 1200);
  const base = await ev("(window.__inp.samples.length ? window.__inp.samples[window.__inp.samples.length-1][2] : -1)");
  const t0 = Date.now();
  await send("Input.dispatchMouseEvent", { type: "mousePressed", x: cx, y: cy, button: "left", clickCount: 1 });
  await send("Input.dispatchMouseEvent", { type: "mouseReleased", x: cx, y: cy, button: "left", clickCount: 1 });
  // poll samples until input byte changes or timeout
  let latency = -1;
  for (let w = 0; w < 60; w++) {
    await sleep(33);
    const s = await ev("JSON.stringify(window.__inp.samples.slice(-40))");
    const samples = JSON.parse(s || "[]");
    const hit = samples.find(x => x[2] !== base && x[0] > 0);
    if (hit) { latency = Date.now() - t0; break; }
  }
  results.push(latency);
  console.log(`click ${i}: input ${base} -> ${latency >= 0 ? latency + 'ms' : 'no change'}`);
  // also grab latest clock sample for G2G context
  const s = await ev("JSON.stringify(window.__inp.samples.slice(-3))");
  const samples = JSON.parse(s || "[]");
  if (samples.length) {
    const [t, clk] = samples[samples.length - 1];
    G2G.push(((Date.now() % 60000) - clk + 60000) % 60000);
  }
  await sleep(400);
}
const ok = results.filter(x => x > 0).sort((a, b) => a - b);
const g = G2G.filter(x => x > 0 && x < 3000).sort((a, b) => a - b);
const stat = (a) => a.length ? { n: a.length, p50: a[Math.floor(a.length * .5)], min: a[0], max: a[a.length - 1], mean: +(a.reduce((x, y) => x + y, 0) / a.length).toFixed(1) } : {};
console.log('input_photon_ms', JSON.stringify(stat(ok)));
console.log('g2g_context_ms', JSON.stringify(stat(g)));
ws.close(); process.exit(0);
