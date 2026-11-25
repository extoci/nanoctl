# nanoctl for iPhone

The native viewer connects directly to an existing nanoctl agent. Video arrives over
unordered/reliable WebRTC data channels (`nanoctl.video.v1`, `nanoctl.video-idr.v1`)
and is hardware-decoded by VideoToolbox into Metal — bypassing the RTP jitter buffer
for lower glass-to-glass latency. H.264 and HEVC take this path; other codecs (e.g.
AV1) fall back to the classic RTP track decoded by libwebrtc. The app also handles
Opus audio + mute, clipboard sync, display selection, touch/pointer, keyboard and
gamepad input, ping keepalive, a stats overlay, and automatic reconnect. See
`docs/PROTOCOL.md` for the wire format and the `video-transport`/`link-stats`
control messages the agent understands.

## Build and connect

1. On a Mac with Xcode 16 or newer and an iOS simulator installed, run
   `brew install xcodegen`, then `cd apps/ios && xcodegen generate`.
2. Open `Nanoctl.xcodeproj` and run the **Nanoctl** scheme. To run on an iPhone
   (iOS 17 or later), select your Signing team in Xcode and use a unique bundle ID if needed.
3. Sign the iPhone and host into the same Tailscale network. Use the agent's full HTTPS
   MagicDNS address, e.g. `https://host.tailnet.ts.net:7337`. No token is needed: the
   agent uses the existing tailnet identity and access policy. Allow the agent port
   and direct WebRTC UDP in tailnet policy and host firewalls. There is no STUN or TURN
   fallback, and the app does not bypass TLS validation.

The desktop fills the screen as soon as you connect. A single glass button in the
top-right corner expands into AirPlay, touch mode and a menu for sound, paste,
display, stream limits (frame rate, bitrate, codec, stats) and disconnect.

Touch defaults to **trackpad** mode, like the web viewer: drag to move the cursor,
tap to click, double-tap and hold to drag, two fingers to scroll or right-click,
three fingers to middle-click, pinch to zoom. **Direct touch** (tap where you want to
click, hold to right-click) is one tap away and remembered. Pair a controller with
iOS to forward its standard controls. The app uses native controller change events
instead of polling; only idle keepalive uses a timer. A DualSense or DualShock profile
requests a **virtual DualShock 4** on a Windows host with ViGEm; other supported
profiles request an Xbox 360. This is not a native DualSense device on the host, and
does not forward rumble, adaptive triggers, touchpad, or motion.

## External display, no tvOS app

When iOS provides a separate external-display scene, nanoctl sends only video there
and keeps the touchpad, gamepad and connection controls on the phone. When the display
disconnects, playback returns to the phone without a second WebRTC session.

For AirPlay, use **Control Center › Screen Mirroring**; the AirPlay button in the
session controls routes sound and explains this if no second screen appears. The split view requires iOS to
expose that AirPlay route as a separate screen. A mirrored-only route cannot show
different phone and TV content; this prototype does not implement a separate AirPlay
transport or promise split view on all receivers. The Simulator can exercise the
external scene, but does not measure AirPlay.

## Repeat the Simulator comparison

Run from the repo root after installing Bun dependencies. The Mac needs Chrome,
Screen Recording and Accessibility permission for the test agent, and one booted
iOS Simulator. Close other capture and build workloads and leave the Mac's Chrome
marker window in front. Ports 7337 and 7338 must be free.

```sh
(cd apps/ios && xcodegen generate)
xcodebuild -project apps/ios/Nanoctl.xcodeproj -scheme Nanoctl \
  -destination "platform=iOS Simulator,id=$SIMULATOR_ID" \
  -derivedDataPath apps/ios/DerivedData -parallel-testing-enabled NO test
bun apps/ios/Benchmark/run.ts \
  apps/ios/DerivedData/Build/Products/Debug-iphonesimulator/Nanoctl.app \
  "$SIMULATOR_ID" /tmp/nanoctl-ios-latency
```

The runner temporarily instruments the generated viewer assets; do not use those
assets or its dev loopback identity in production. It uses the browser suite's marker
and agent launcher, pacing the source marker at 30 fps and using the same software
H.264 settings for both viewers.
It reports unique visible markers per second and p50/p95 for host draw → estimated
presentation and scripted Space dispatch → first response presentation. The native
probe only inspects drawables submitted for presentation whose GPU commands complete.
The Simulator does not expose a Metal presentation timestamp, so the probe estimates
the next eligible refresh after the GPU completion callback. The display link supplies
the refresh phase; normal rendering does not wait for a display-link tick.
Safari uses `requestVideoFrameCallback`'s `expectedDisplayTime`. Clock timestamps
share the Mac's epoch. Neither measures photons. The input test does not measure
Bluetooth or compare GameController callbacks with Safari's gamepad polling.

Set `NANOCTL_SAFARI_FIRST=1` to reverse the run order, or
`NANOCTL_RENDER_ON_VSYNC=1` to reproduce the old native polling scheduler with
the same instrumentation. The runner saves raw samples and a screenshot of each
viewer alongside the summary. Native samples include decoder entry and callback,
mailbox delivery, render start, submission and GPU completion callback. The
summary also reports interval averages from inbound WebRTC counters (actual,
target and minimum jitter-buffer residence, and decode time).

For per-frame receive timing on this Mac-only loopback workload, start this
capture in another terminal before the benchmark, and set
`NANOCTL_RTP_TRACE=/tmp/nanoctl-rtp.pcap` for the runner:

```sh
sudo tcpdump -i lo0 -y NULL -n -U -s 44 -w /tmp/nanoctl-rtp.pcap \
  'ip and udp and src host 127.0.0.1 and udp[8] & 0xc0 = 0x80 and udp[9] & 0x7f >= 96'
```

Stop the capture afterward. This recipe saves IPv4 RTP headers, not video payloads;
the probe joins the loopback timestamp of each end-of-frame RTP packet to its native
decoder timestamps by SSRC and RTP timestamp. Safari uses video-frame metadata for
receive and processing timing when WebKit supplies those fields.
