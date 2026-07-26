import { expect, test } from "@playwright/test";

test.beforeEach(async ({ page }) => {
  await page.addInitScript(() => {
    const fixture = window as typeof window & {
      __nanoctlChannels?: { label: string; payloads: string[]; closed: boolean }[];
      __nanoctlPeers?: {
        closed: boolean;
        setState: (state: RTCPeerConnectionState) => void;
      }[];
      __nanoctlViewerEvents?: unknown[];
    };
    fixture.__nanoctlChannels = [];
    fixture.__nanoctlPeers = [];
    fixture.__nanoctlViewerEvents = [];

    class FixtureDataChannel {
      readonly payloads: string[] = [];
      readonly bufferedAmount = 0;
      readonly readyState = "open";
      closed = false;

      constructor(readonly label: string) {
        fixture.__nanoctlChannels?.push(this);
      }

      send(payload: string) {
        this.payloads.push(payload);
      }

      close() {
        this.closed = true;
      }
    }

    class FixturePeer {
      connectionState: RTCPeerConnectionState = "new";
      signalingState: RTCSignalingState = "stable";
      onconnectionstatechange: (() => void) | null = null;
      onicecandidate: (() => void) | null = null;
      ontrack: (() => void) | null = null;
      closed = false;

      constructor() {
        fixture.__nanoctlPeers?.push(this);
      }

      createDataChannel(label: string) {
        return new FixtureDataChannel(label);
      }

      addTransceiver() {}

      async createOffer() {
        return { type: "offer", sdp: "v=0\r\n" };
      }

      async setLocalDescription() {}

      async setRemoteDescription() {}

      async addIceCandidate() {}

      async getStats() {
        return new Map();
      }

      setState(state: RTCPeerConnectionState) {
        this.connectionState = state;
        this.onconnectionstatechange?.();
      }

      close() {
        this.closed = true;
        this.connectionState = "closed";
        this.signalingState = "closed";
        this.onconnectionstatechange?.();
      }
    }

    Object.defineProperty(window, "RTCPeerConnection", {
      configurable: true,
      value: FixturePeer,
    });
  });
});

test("viewer switches displays and releases control with button and emergency shortcut", async ({
  page,
}) => {
  await page.goto("/e2e/viewer");
  await expect(page.getByText("Negotiating")).toBeVisible();
  const display = page.getByLabel("Display");
  await expect(display).toHaveValue("display-primary");
  await display.selectOption("display-secondary");
  await expect
    .poll(() =>
      page.evaluate(() =>
        (
          window as typeof window & {
            __nanoctlChannels?: { label: string; payloads: string[] }[];
          }
        ).__nanoctlChannels
          ?.find((channel) => channel.label === "nanoctl.control.v1")
          ?.payloads.some((payload) => payload.includes('"type":"display"')),
      ),
    )
    .toBe(true);

  await page.getByRole("button", { name: "Control: on" }).click();
  await expect(page.getByRole("button", { name: "Control: off" })).toBeVisible();
  await expect(display).toBeDisabled();
  await page.getByRole("button", { name: "Control: off" }).click();
  await page.keyboard.press("Control+Alt+Shift+Escape");
  await expect(page.getByRole("button", { name: "Control: off" })).toBeVisible();
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (
            window as typeof window & {
              __nanoctlChannels?: { label: string; payloads: string[] }[];
            }
          ).__nanoctlChannels
            ?.find((channel) => channel.label === "nanoctl.control.v1")
            ?.payloads.filter((payload) => payload === '{"type":"release"}').length,
      ),
    )
    .toBeGreaterThanOrEqual(2);
});

test("viewer cleanup releases input, closes channels, and closes the peer", async ({ page }) => {
  await page.goto("/e2e/viewer");
  await page.getByRole("button", { name: "Unmount viewer" }).click();
  await expect(page.getByText("Viewer unmounted")).toBeVisible();
  const evidence = await page.evaluate(() => {
    const fixture = window as typeof window & {
      __nanoctlChannels?: { payloads: string[]; closed: boolean }[];
      __nanoctlPeers?: { closed: boolean }[];
    };
    return {
      released: fixture.__nanoctlChannels?.some((channel) =>
        channel.payloads.includes('{"type":"release"}'),
      ),
      channelsClosed: fixture.__nanoctlChannels?.every((channel) => channel.closed),
      peerClosed: fixture.__nanoctlPeers?.every((peer) => peer.closed),
    };
  });
  expect(evidence).toEqual({ released: true, channelsClosed: true, peerClosed: true });
});

test("viewer exhausts bounded reconnect attempts and ends the session", async ({ page }) => {
  await page.goto("/e2e/viewer");
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (
            window as typeof window & {
              __nanoctlPeers?: unknown[];
            }
          ).__nanoctlPeers?.length,
      ),
    )
    .toBe(1);
  for (let attempt = 1; attempt <= 3; attempt += 1) {
    await page.evaluate(() => {
      (
        window as typeof window & {
          __nanoctlPeers?: { setState: (state: RTCPeerConnectionState) => void }[];
        }
      ).__nanoctlPeers?.[0]?.setState("failed");
    });
    await expect(page.getByText(`reconnecting (${attempt}/3)`)).toBeVisible();
  }
  await page.evaluate(() => {
    (
      window as typeof window & {
        __nanoctlPeers?: { setState: (state: RTCPeerConnectionState) => void }[];
      }
    ).__nanoctlPeers?.[0]?.setState("failed");
  });
  await expect(page.getByText("failed", { exact: true })).toBeVisible();
  await expect
    .poll(() =>
      page.evaluate(() =>
        (
          window as typeof window & {
            __nanoctlViewerEvents?: { operation: string; reason?: string }[];
          }
        ).__nanoctlViewerEvents?.some(
          (event) => event.operation === "end" && event.reason === "controller connection failed",
        ),
      ),
    )
    .toBe(true);
});

test("viewer handles terminal state and pagehide cleanup", async ({ page }) => {
  await page.goto("/e2e/viewer?state=ended");
  await expect(page.getByText("ended: host stopped")).toBeVisible();
  await page.evaluate(() => window.dispatchEvent(new PageTransitionEvent("pagehide")));
  await expect
    .poll(() =>
      page.evaluate(() =>
        (
          window as typeof window & {
            __nanoctlViewerEvents?: { operation: string; reason?: string }[];
          }
        ).__nanoctlViewerEvents?.some(
          (event) => event.operation === "end" && event.reason === "controller disconnected",
        ),
      ),
    )
    .toBe(true);
});
