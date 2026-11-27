import { chromium, expect, test, type Page } from "@playwright/test";
import {
  encodeInputMessage,
  VIDEO_DELTA_CHANNEL_LABEL,
  VIDEO_REFERENCE_CHANNEL_LABEL,
  CONTROL_CHANNEL_LABEL,
} from "../../packages/protocol/src/index";

interface VideoFaults {
  drop: boolean;
  dropped: number;
  reorder: boolean;
  reordered: number;
  requests: number;
  control?: RTCDataChannel;
}

declare global {
  interface Window {
    videoFaults: VideoFaults;
  }
}

async function installVideoFaults(page: Page) {
  await page.addInitScript(
    ({ delta, reference, control }) => {
      const faults: VideoFaults = {
        drop: false,
        dropped: 0,
        reorder: false,
        reordered: 0,
        requests: 0,
      };
      window.videoFaults = faults;
      const create = RTCPeerConnection.prototype.createDataChannel;
      RTCPeerConnection.prototype.createDataChannel = function (...args) {
        const channel = Reflect.apply(create, this, args) as RTCDataChannel;
        if (channel.label === control) faults.control = channel;
        if (channel.label === reference) {
          const send = channel.send;
          channel.send = function (data: unknown) {
            if (ArrayBuffer.isView(data) && data.byteLength === 2) {
              const bytes = new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
              if (bytes[0] === 4 && bytes[1] === 0x41) faults.requests += 1;
            }
            Reflect.apply(send, this, [data]);
          };
        }
        if (channel.label === delta) {
          let held: ArrayBuffer | undefined;
          channel.addEventListener("message", (event) => {
            // Leave browser-generated messages on the real channel. Only the
            // two synthetic replay events bypass this receive-side fault hook.
            if (!event.isTrusted || !(event.data instanceof ArrayBuffer)) return;
            if (faults.drop) {
              event.stopImmediatePropagation();
              faults.dropped += 1;
              return;
            }
            if (!faults.reorder || new DataView(event.data).getUint16(8, true) !== 1) return;
            event.stopImmediatePropagation();
            if (held === undefined) {
              held = event.data;
              return;
            }
            faults.reorder = false;
            faults.reordered += 1;
            channel.dispatchEvent(new MessageEvent("message", { data: event.data }));
            channel.dispatchEvent(new MessageEvent("message", { data: held }));
            held = undefined;
          });
        }
        return channel;
      };
    },
    {
      delta: VIDEO_DELTA_CHANNEL_LABEL,
      reference: VIDEO_REFERENCE_CHANNEL_LABEL,
      control: CONTROL_CHANNEL_LABEL,
    },
  );
}

test("v4 input reaches the host during video loss and the viewer recovers after loss stops", async ({
  page,
}) => {
  test.skip(
    process.env.NANOCTL_BROWSER_MEDIA !== "1" ||
      process.platform !== "linux" ||
      !process.env.DISPLAY,
    "Requires Linux native input and live capture",
  );
  await installVideoFaults(page);
  const hostBrowser = await chromium.launch({ headless: false });
  try {
    const host = await hostBrowser.newPage();
    const input = host.getByRole("textbox");
    await host.setContent(`<input aria-label="Native input target" autofocus>
      <span style="display:inline-block; width:80px; height:30px; background:cyan;
        animation: move 900ms linear infinite alternate"></span>
      <style>@keyframes move { to { transform:translateX(120px) } }</style>`);
    await host.bringToFront();
    await input.focus();
    await page.goto("/");
    await expect(page.getByTestId("connection-state")).toHaveText("Live");
    await page.waitForFunction(() => window.videoFaults.control?.readyState === "open");
    const presented = () =>
      page
        .locator("video")
        .evaluate((node) => (node as HTMLVideoElement).getVideoPlaybackQuality().totalVideoFrames);

    const beforeReorder = await presented();
    await page.evaluate(() => {
      window.videoFaults.reorder = true;
    });
    await expect.poll(() => page.evaluate(() => window.videoFaults.reordered)).toBe(1);
    await expect.poll(presented).toBeGreaterThan(beforeReorder + 3);

    await page.evaluate(() => {
      window.videoFaults.drop = true;
    });
    await expect
      .poll(() => page.evaluate(() => window.videoFaults.dropped))
      .toBeGreaterThanOrEqual(4);
    const beforeInput = await page.evaluate(() => window.videoFaults.dropped);
    const text = "input during video loss";
    await page.evaluate(
      (wire: number[]) => {
        window.videoFaults.control!.send(new Uint8Array(wire));
      },
      Array.from(encodeInputMessage({ kind: "text", sequence: 10_000, text })),
    );
    await expect(input).toHaveValue(text, { timeout: 1_500 });
    await expect
      .poll(() => page.evaluate(() => window.videoFaults.dropped))
      .toBeGreaterThan(beforeInput);

    const beforeRecovery = await presented();
    const requests = await page.evaluate(() => {
      window.videoFaults.drop = false;
      return window.videoFaults.requests;
    });
    await expect
      .poll(() => page.evaluate(() => window.videoFaults.requests), { timeout: 2_000 })
      .toBeGreaterThan(requests);
    await expect.poll(presented, { timeout: 2_000 }).toBeGreaterThan(beforeRecovery + 3);
    await expect(page.getByTestId("connection-state")).toHaveText("Live");
  } finally {
    await hostBrowser.close();
  }
});
