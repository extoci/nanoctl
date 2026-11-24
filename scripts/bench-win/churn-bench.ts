const id = process.argv[2],
  n = parseInt(process.argv[3] || "8");
const info = await (await fetch("http://localhost:9222/json/list")).json();
const page = info.find((p) => p.id === id);
if (!page) {
  console.log("no page");
  process.exit(1);
}
const ws = new WebSocket(page.webSocketDebuggerUrl);
await new Promise((r) => (ws.onopen = r));
let mid = 0;
const pend = new Map();
ws.onmessage = (e) => {
  const m = JSON.parse(e.data);
  if (m.id && pend.has(m.id)) {
    pend.get(m.id)(m);
    pend.delete(m.id);
  }
};
const ev = async (expr) => {
  const i = ++mid;
  ws.send(
    JSON.stringify({
      id: i,
      method: "Runtime.evaluate",
      params: { expression: expr, returnByValue: true },
    }),
  );
  const r = await new Promise((r) => pend.set(i, r));
  return r.result?.result?.value;
};
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
for (let i = 0; i < n; i++) {
  await ev("location.reload()");
  await sleep(2500);
  const r = await ev("({j:window.__dcv?.joinMs??-1,f:window.__dcv?.firstDecodeMs??-1})");
  await sleep(3000);
  const live = await ev("document.body.innerText.includes('Live')");
  const d = await ev(
    "({dec:window.__dcv?.decoded??0,kf:window.__dcv?.kf??0,gaps:window.__dcv?.gaps??0})",
  );
  console.log(
    `attach ${i}: join=${r.j}ms firstDecode=${r.f}ms live=${live} decoded=${d.dec} kf=${d.kf} gaps=${d.gaps}`,
  );
}
ws.close();
process.exit(0);
