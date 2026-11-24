// Measure join latency: reload the viewer page, wait for the DC-video
// path to attach + first keyframe, read window.__dcv.joinMs.
const page = process.argv[2];
if (!page) {
  console.error("usage: bun join-bench.ts <page-id> [baseline]");
  process.exit(2);
}
const ws = new WebSocket(`ws://localhost:9222/devtools/page/${page}`);
let id = 0;
const pending = new Map<number, (v: any) => void>();
const send = (method: string, params: any = {}) =>
  new Promise<any>((resolve) => {
    const mid = ++id;
    pending.set(mid, resolve);
    ws.send(JSON.stringify({ id: mid, method, params }));
  });
const evalJs = async (expr: string) =>
  (await send("Runtime.evaluate", { expression: expr, returnByValue: true })).result?.result?.value;
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

ws.onmessage = (e) => {
  const m = JSON.parse(e.data);
  if (m.id && pending.has(m.id)) {
    pending.get(m.id)!(m);
    pending.delete(m.id);
  }
};
await new Promise((r) => (ws.onopen = r));
const variant = process.argv[3]; // "baseline" disables the early-KF send
if (variant === "baseline") {
  await send("Page.addScriptToEvaluateOnNewDocument", {
    source: "window.__noEarlyKf = true;",
  });
} else {
  await send("Page.addScriptToEvaluateOnNewDocument", {
    source: "window.__noEarlyKf = false;",
  });
}
await send("Page.reload", { ignoreCache: false });
await sleep(1500);
let out: any = null;
for (let i = 0; i < 60; i++) {
  const v = await evalJs(
    "JSON.stringify({joinMs: window.__dcv?.joinMs, firstMsgMs: window.__dcv?.firstMsgMs, firstDecodeMs: window.__dcv?.firstDecodeMs, msgs: window.__dcv?.msgs, fed: window.__dcv?.fed, waiting: window.__dcv?.waitingForKey})",
  );
  if (v) out = JSON.parse(v);
  if (out?.firstDecodeMs) break;
  await sleep(500);
}
console.log(JSON.stringify(out));
ws.close();
process.exit(0);
