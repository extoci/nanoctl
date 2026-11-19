# Cross-product X11 benchmark

Linux-only software input-to-client-pixels benchmark. The same headed Chromium
source app and persistent XShm detector work with browser and native viewers.
No product protocol or decoded-video timestamp is used for the headline results.
This is not an optical input-to-photon measurement and excludes physical devices,
monitor scanout, and network conditions beyond the configured connection.

## Prerequisites

Run from the repository root after `bun install --frozen-lockfile` and
`bunx playwright install chromium`. Native requirements:

```sh
sudo apt-get install build-essential libx11-dev libxext-dev libxtst-dev xvfb xdotool
```

Use **separate** 1280x720x24 Xvfb displays for source and client, with no window
manager, scaling, other visible windows, or competing streaming products. Start
your product's host on the source display. Use software H.264. Keep product
defaults otherwise, including the default 60-fps ceiling; record actual settings
and bitrate. Audio remains at product defaults. Build nanoctl's release media
agent and viewer before running (`bun run build:agent`).

```sh
Xvfb :97 -screen 0 1280x720x24 -nolisten tcp &
Xvfb :94 -screen 0 1280x720x24 -nolisten tcp &
# Start nanoctl on :97 with a config allowing dev@example.com, tls=off,
# max_width=1280, max_height=720, encoder=software (default max_fps=60).
DISPLAY=:97 NANOCTL_DEV_LOOPBACK_USER=dev@example.com \
  crates/nanoctl-agent/target/release/nanoctl --config "$HOME/nanoctl-bench.toml" run &
HOST_PID=$!
VIEWER_URL=http://127.0.0.1:7338 \
RUNTIME_PATHS="$PWD/crates/nanoctl-agent/target/release/nanoctl" \
BITRATE_NOTE='24 Mbps default max; measured inbound RTP in raw.webRtcStats' \
  bun scripts/bench/run.ts nanoctl :97 :94 "$HOST_PID" "$HOME/bench-results"
```

`VIEWER_URL` starts a headed browser on the client display. `VIEWER_SELECTOR`
defaults to `video`; set it to `canvas` or the appropriate input surface for
non-video browser viewers. Browser windows are explicitly sized to the display;
browser chrome and viewer controls remain part of the default viewing experience.
Without `VIEWER_URL`,
the runner attaches to an already streaming native viewer. For Moonlight, first
pair with Sunshine, then start a 720p software H.264 stream on its own X display:

```sh
# Sunshine host on :96: capture=x11 encoder=software system_tray=disabled
# Existing paired Moonlight installation, with host base port 49000:
DISPLAY=:95 XDG_RUNTIME_DIR=/run/user/1000 SDL_RENDER_DRIVER=software \
LIBGL_ALWAYS_SOFTWARE=1 "$HOME/Downloads/squashfs-root/AppRun" stream \
  --720 --video-codec H.264 --video-decoder software \
  --display-mode windowed 127.0.0.1:49000 Desktop &
# A 1280x720 client display and undecorated, full-size window are required.
RUNTIME_PATHS=/usr/bin/sunshine:/usr/share/sunshine \
BITRATE_NOTE='record Sunshine Streaming bitrate and Moonlight Video bitrate logs' \
  bun scripts/bench/run.ts moonlight :96 :95 SUNSHINE_PID "$HOME/bench-results"
```

The client must already be connected when attaching. The runner clicks its
center, explicitly focuses the named browser window or largest visible native
X window (needed without a window manager), and checks that the static source marker is decodable. Set `BENCH_WINDOW` to an explicit native X window ID when needed. The
detector locates the color-guard row in an initial full-screen capture, then
reads only that single row. It fails rather than returning bogus results if
geometry is wrong.
Browser and native clients must forward Space to the source. In particular,
verify input permissions on Sunshine; video streaming alone is insufficient.
Do not run these trials on a display with sensitive applications.

## Method and outputs

### Optional nanoctl stage attribution

Restart the Linux agent with `NANOCTL_BENCH_TRACE=1` and redirect its output to
a fresh persistent log. Then run the browser benchmark with
`BENCH_ATTRIBUTION=1`. For example, using the displays/config above:

```sh
DISPLAY=:97 NANOCTL_DEV_LOOPBACK_USER=dev@example.com NANOCTL_BENCH_TRACE=1 \
  crates/nanoctl-agent/target/release/nanoctl --config "$HOME/nanoctl-bench.toml" run \
  > "$HOME/bench-results/trace.agent.log" 2>&1 &
HOST_PID=$!
BENCH_ATTRIBUTION=1 VIEWER_URL=http://127.0.0.1:7338 \
  bun scripts/bench/run.ts trace :97 :94 "$HOST_PID" "$HOME/bench-results"
bun scripts/bench/attribution.ts "$HOME/bench-results/trace" \
  "$HOME/bench-results/trace.agent.log"
```

This opt-in diagnostic decodes the benchmark marker in captured/native and
viewer frames, joins capture/encode/write records to source IDs, and records
Space down receive/injection ordinals without recording arbitrary key contents.
It is disabled by default. Native timestamps use CLOCK_MONOTONIC; source and
viewer performance clocks are aligned independently through shortest-round-trip
samples. The native marker row and viewer crop are discovered rather than
assuming a browser-chrome offset. Restart between runs to reset input ordinals;
do not inject extra Space presses. The encoded-frame lookup and viewer frame
collection are bounded.

The analyzer writes `.attribution.json` and `.attribution.md`, with individual
segment counts and p50/p95. Missing native/receiver metadata is not fabricated.
Negative deltas are retained and counted: browser clock uncertainty and
`expectedDisplayTime` prediction errors must not be interpreted as physical
negative latency. `processingDuration` is decoder work, not an independently
observed decode-end timestamp. Capture is sampled after XDamage receipt, so
damage-to-source association can be coalesced; this is a diagnostic side channel,
not the product-neutral headline measurement. Instrumented runs include tracing
and one-row video readback overhead; compare with uninstrumented runs too.

### Common measurements

- The source reuses `tests/browser/latency-marker.ts` encoding (16-bit source
  frame, 8-bit input counter, checksum, color guards). Animation follows host
  requestAnimationFrame, approximately 60 Hz. Space increments only on a trusted
  keydown. The idle phase redraws nothing; motion adds a moving rectangle.
- The C helper attaches XShm once, captures a 1280x1 root-window strip every
  **4 ms (250 Hz target)**, and decodes the marker. CLOCK_MONOTONIC timestamps
  follow synchronous pixel acquisition. Input timestamps precede XTest Space
  press/release dispatch into the focused client. Absolute-time sleeps limit
  accumulated scheduling drift. Each run uses 120 trials with pseudorandom
  250–450 ms gaps, seed 1729; `TRIALS` accepts 100–250.
- Input latency is the first captured input counter minus injection time.
  Video latency maps the source's canvas drawing timestamp onto CLOCK_MONOTONIC
  using the shortest of 20 browser round trips. The Bun runner calls libc's
  `clock_gettime` through built-in FFI: Bun's process-relative `hrtime` must not
  be assumed to share the native helper's clock origin. These are draw-to-pixels, not
  source-display scanout timestamps. Polling quantization adds up to roughly
  4 ms under normal scheduling, plus capture time. JSON includes all polling
  gaps, read/work distributions, detector CPU/RSS, and clock uncertainty.
- FPS counts distinct source markers, not repeated polls. Inter-frame intervals
  and stalls >100 ms exclude the first second of motion. Percentiles use nearest
  rank. Missing responses are explicit. A valid run requires >=100 matched inputs,
  no missing inputs, the expected source input count, and >=200 actual polls/s.
- Host `/proc` process-tree CPU and RSS are sampled every 250 ms, averaged over
  a 10-second static phase and the motion phase. CPU is **one core = 100%**.
  RSS sums the host tree (shared pages may count twice), excluding source app,
  viewer, X server, and detector. Short-lived children between polls can be missed.
  The detector's own CPU is reported separately; client X-server capture overhead
  is not included in that number. Idle source Chrome still runs a nonpainting RAF.
- `RUNTIME_PATHS` measures logical bytes of the specified installed host binary
  and required assets. It excludes caches, client installs, and system shared
  libraries; list those separately if comparing full deployment footprints.
  Nanoctl embeds viewer assets in its binary. Sunshine needs `/usr/share/sunshine`.
- Browser runs save inbound WebRTC getStats snapshots as a diagnostic side
  channel. Estimate received media bitrate from consecutive bytesReceived and
  timestamp deltas; configured encoder ceilings are not measured media bitrate.

Each run writes `PRODUCT.json`, `PRODUCT.md`, `PRODUCT.pixels.log`, and
`PRODUCT.source.json` into the output directory, plus the compiled helper.
Keep outputs outside the repository or copy selected results intentionally.
The JSON includes raw source timestamps, per-phase host samples, distributions,
sample counts and validity. The pixel log includes every poll and injection.
Run products sequentially; the harness closes browsers it owns but leaves host
servers and externally started native clients running.

Tests: `bun test scripts/bench/metrics.test.ts scripts/latency-marker.test.ts`.
The runner compiles the helper with `-Wall -Wextra -Werror` on every invocation.
