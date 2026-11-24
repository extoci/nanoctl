// Measure viewer session startup: navigation -> signaling socket created ->
// server hello -> RTCPeerConnection "connected", via an init script that
// timestamps the browser APIs (performance.now() is relative to navigation).
// usage: bun connect-bench.ts <base-url> [N]
// Pair with `tc qdisc add dev lo root netem delay <ms>` to model tailnet RTT.
import { chromium } from "@playwright/test";

const base = process.argv[2];
const runs = Number(process.argv[3] ?? 10);
if (!base) {
  console.error("usage: bun connect-bench.ts <base-url> [N]");
  process.exit(2);
}
const probe = () => {
  const marks: Record<string, number> = {};
  (window as any).__connect = marks;
  const NativeSocket = window.WebSocket;
  window.WebSocket = class extends NativeSocket {
    constructor(url: string | URL, protocols?: string | string[]) {
      super(url, protocols);
      marks.ws ??= performance.now();
      this.addEventListener("message", () => (marks.hello ??= performance.now()), { once: true });
    }
  };
  const NativePeer = window.RTCPeerConnection;
  window.RTCPeerConnection = class extends NativePeer {
    constructor(configuration?: RTCConfiguration) {
      super(configuration);
      marks.peer ??= performance.now();
      this.addEventListener("connectionstatechange", () => {
        if (this.connectionState === "connected") marks.connected ??= performance.now();
      });
    }
  };
};
const browser = await chromium.launch();
const context = await browser.newContext();
await context.addInitScript(probe);
const rows: Record<string, number>[] = [];
for (let i = 0; i < runs; i++) {
  const page = await context.newPage();
  await page.goto(base);
  await page.waitForFunction(() => (window as any).__connect?.connected !== undefined, null, {
    timeout: 20_000,
  });
  rows.push(await page.evaluate(() => (window as any).__connect));
  await page.close();
  await new Promise((resolve) => setTimeout(resolve, 300));
}
await browser.close();
const median = (values: number[]) => {
  const sorted = [...values].sort((a, b) => a - b);
  return sorted[Math.floor(sorted.length / 2)]!;
};
for (const key of ["ws", "hello", "peer", "connected"]) {
  const values = rows.map((row) => row[key]!);
  console.log(
    `${key.padEnd(9)} median ${median(values).toFixed(1)}ms  min ${Math.min(...values).toFixed(1)}  max ${Math.max(...values).toFixed(1)}`,
  );
}
