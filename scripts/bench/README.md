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
`VIEWER_SETUP_CLICKS` is an optional JSON array of CSS selectors to click in order
after navigation, before focusing the streaming surface. Use real UI controls to
grant input permissions rather than modifying viewer internals. Select a unique
streaming canvas when the viewer also has a cursor/overlay canvas.
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

## screen-relay in Ubuntu 24.04 Docker

The [upstream Docker guide](https://screen-relay.dev/docs/docker) builds its own
desktop image. This benchmark instead pins the npm release to 0.1.2, builds its
unmodified X11 engine on Ubuntu 24.04, and uses an existing 720p Xvfb display with
no desktop. The Node 24 base supplies Node/npm; `screen-relay install` installs
the native dependencies and compiles the engine. Run from the repository root:

```sh
docker build -f scripts/bench/screen-relay.Dockerfile -t nanoctl-screen-relay:0.1.2 scripts/bench
docker run -d --name nanoctl-screen-relay --network host --ipc host \
  -v /tmp/.X11-unix:/tmp/.X11-unix nanoctl-screen-relay:0.1.2
docker exec -d nanoctl-screen-relay Xvfb :93 -screen 0 1280x720x24 -nolisten tcp -ac
docker exec nanoctl-screen-relay screen-relay up --backend x11 --desktop none \
  --display :93 --bind 127.0.0.1 --port 8088 --detach --timeout 30
ENGINE_PID=$(docker top nanoctl-screen-relay -eo pid,comm | awk '$2 == "relay-engine" { print $1 }')
```

Use unused display/port/container names. Only loopback HTTP is exposed. This
trusted local benchmark shares the X socket and IPC namespace so the **same VM
Chromium source** can use the container's display, including MIT-SHM. This keeps
the source browser identical across products; the engine and Xvfb are inside the
container, the source browser and headed viewer are on the VM. The client Xvfb
`:94` and persistent XShm detector remain on the VM as usual.

The viewer is WebCodecs -> `canvas#screen` (a second canvas draws the cursor).
It starts **view-only**. Open the toolbar and click Take control; the harness
then focuses/clicks the canvas before trusted XTest Space trials. Ordinary click
release preserves canvas focus. No codec, frame pacing, buffering or bitrate
settings are overridden. Defaults: software x264 H.264, 60 fps, min/start/max
bitrate 1/8/40 Mbps. `/metrics` exposes the current encoder target, not measured
viewer throughput; its cgroup CPU/RAM are **not** the process-tree measurements.

### Comparable runtime sizes, including linked libraries

To count container files without relying on `/proc/PID/root` access, export an
immutable runtime snapshot before measurement. The helper refuses an existing
destination or manifest. Explicit executable roots get their transitive `ldd`
dependencies; asset directories are copied whole. Pass native addons as explicit
ELF roots too if a different product needs them. Missing libraries fail the
export. The adjacent `.json` manifest records roots, dependency paths and bytes.

```sh
mkdir -p "$HOME/bench-runtime"
bun scripts/bench/runtime.ts --container nanoctl-screen-relay "$HOME/bench-runtime/screen-relay" \
  /usr/local/lib/node_modules/screen-relay /root/.local/share/screen-relay/engine/0.1.2/relay-engine \
  /usr/local/bin/node /usr/bin/setpriv /usr/bin/xdpyinfo /usr/bin/xprop \
  /usr/bin/xwininfo /usr/bin/setxkbmap /usr/bin/xauth
bun scripts/bench/runtime.ts "$HOME/bench-runtime/nanoctl" \
  "$PWD/crates/nanoctl-agent/target/release/nanoctl"

VIEWER_URL=http://127.0.0.1:8088 VIEWER_SELECTOR='#screen' \
VIEWER_SETUP_CLICKS='["#toolbar-toggle","[data-action=control]"]' \
RUNTIME_PATHS="$HOME/bench-runtime/screen-relay" \
RUNTIME_EXCLUDES='Other OS files, Xvfb, browsers, toolchain, caches; linked ELF runtime libraries included' \
BITRATE_NOTE='defaults: 1/8/40 Mbps min/start/max, software x264, 60 fps' \
  bun scripts/bench/run.ts screen-relay :93 :94 "$ENGINE_PID" "$HOME/bench-results" --repeat 5

# After screen-relay finishes, repeat nanoctl using its HOST_PID and :97 source.
VIEWER_URL=http://127.0.0.1:7338 RUNTIME_PATHS="$HOME/bench-runtime/nanoctl" \
RUNTIME_EXCLUDES='Other OS files, Xvfb, browsers, toolchain, caches; linked ELF runtime libraries included' \
BITRATE_NOTE='default 24 Mbps max; received RTP in raw.webRtcStats' \
  bun scripts/bench/run.ts nanoctl :97 :94 "$HOST_PID" "$HOME/bench-results" --repeat 5
```

Runtime bytes count logical file lengths, deduplicating symlink aliases and
overlapping paths. Both snapshots include their ELF shared-library closure;
screen-relay additionally includes its npm package (bundled viewer, sources and
JS dependencies), built engine, Node runtime and the listed CLI helpers. Other
base-OS utilities, browsers, Xvfb, development packages, caches, and Docker layers
are excluded. This differs from older app-only sizes; do not compare them without
normalizing the counting scope. The full Docker image retains build tools and is
not the runtime-size figure. Repeat summaries include runtime MiB; absent sizes
remain `n/a` and do not by themselves invalidate timing results.

CPU/RSS use the engine's **host-namespace PID and descendants**, not the container
init, Xvfb, source browser or parent Node CLI supervisor. The supervisor remains
running but is deliberately outside the requested engine process tree. Container
cleanup: `docker exec nanoctl-screen-relay screen-relay down`, then
`docker stop nanoctl-screen-relay`.

## Method and outputs

### Repeated comparisons

Append `--repeat N` (1–100) to the usual command. For example:

```sh
VIEWER_URL=http://127.0.0.1:7338 RUNTIME_PATHS="$PWD/crates/nanoctl-agent/target/release/nanoctl" \
  bun scripts/bench/run.ts current :97 :94 "$HOST_PID" "$HOME/bench-results" --repeat 5
```

Each trial creates fresh source/viewer browsers and performs the full idle and
motion phases with 120 input trials. The host process persists across repeats.
Raw artifacts use `PRODUCT-run-1` through `PRODUCT-run-N`; existing run JSON is
refused to prevent overwrites or reuse of stale results after a failed child.
`PRODUCT.repeat.json` and `.repeat.md` report the median of **per-run** p50/p95,
CPU, RSS and FPS, with **min–max** spread. These are not pooled percentiles or
confidence intervals. Every run must be valid before a comparison is complete;
failed/missing runs remain explicit and the repeat command exits nonzero.

Use distinct product labels/output directories for each candidate. Run each
candidate's repeats sequentially, without other active streams or builds, and
record candidate order and binary identity. Keep settings and input seed fixed.
For five-run selection, require lower median input and video p50, with no higher
median input/video p95 or motion CPU than the current pacer. Overlapping ranges
still limit the strength of any conclusion from only five trials.

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

### Reconnect memory probe

On Linux, start a fresh agent on the host display, then run:

```sh
HOST_DISPLAY=:97 CLIENT_DISPLAY=:94 \
  bun scripts/bench/reconnect.ts AGENT_PID "$HOME/bench-results/reconnect.json"
```

This opens the real viewer in a fresh headed-browser context for each of 20
sessions against a shared 720p/60 Hz marker source. Each cycle verifies decoded
video, runs ten seconds of motion, closes the context/WebSocket, waits three
seconds for teardown, and samples agent-only `/proc/PID/status` RSS, thread count
and open FDs. Connected samples and frame counts are also saved after every cycle;
partial artifacts survive errors. Existing output files are refused. Browser
processes persist across cycles but peer connections and contexts do not.

`VIEWER_URL`, `CYCLES`, `MOTION_MS` (at least 1000), and `SETTLE_MS` override the
defaults. Use fresh agents for before/after comparisons; keep the same PID only
when explicitly extending a series. RSS measures physical residency, not live
heap allocation. On glibc Linux, session teardown now returns free heap pages via
`malloc_trim(0)` after media workers have joined and the closed peer is dropped.
It runs on a blocking worker, never in capture/encode or Tokio's async executor.
Other allocators/platforms are unchanged.

### Real content and picture quality

Isolated 720p conversion comparison (no capture, codec, allocation or network in
the timed loop):

```sh
cargo test --release --features media --manifest-path crates/nanoctl-agent/Cargo.toml \
  benchmark_rgba_conversion_720p -- --ignored --nocapture --test-threads=1
```

It rotates three kernels over nine batches of 50 frames: current production
`I420Frame::read_rgba` truncation,
the previously rejected signed rounding, and a test-only unsigned-16-bit rounded
candidate. Output is median/min/max ns/frame, with reusable input/output and
`black_box`. Candidate kernels are checked byte-for-byte against the reference;
they are not enabled in the product merely because the microbench passes.

For software-encoder diagnosis, start the agent with
`RUST_LOG=info,nanoctl::encoder=debug`. Each encode attempt logs its input-frame
count, last encoded frame's average macroblock QP, current target kbps, and output
bytes. Zero-byte attempts are skipped frames, not new QP observations. These
queries and per-frame logs are disabled at the normal info level; disable them
for resource comparisons.

Browser `raw.webRtcStats` includes nominated candidate pairs and transports as
well as inbound video. Inspect `selectedCandidatePairId`, packet/byte activity,
and optional `availableIncomingBitrate` together: nomination alone does not mean
the pair carries video, and a missing bandwidth field is not zero capacity.
Received media Mbps uses only inbound video RTP counter deltas, never candidate
pair or transport bytes. Invalid/reset video counters fail the run rather than
producing a misleading rate.

`BENCH_WORKLOAD=text|drag|gradient` selects deterministic code editing/scrolling,
large editor-window motion, or an animated color-gradient scene. The default
`marker` workload remains unchanged. All retain the same checksummed source IDs.

```sh
BENCH_QUALITY=1 BENCH_WORKLOAD=text VIEWER_URL=http://127.0.0.1:7338 \
  bun scripts/bench/run.ts content-nanoctl-text :97 :94 AGENT_PID \
    "$HOME/bench-results/content" --repeat 3

BENCH_QUALITY=1 BENCH_WORKLOAD=text VIEWER_URL=http://127.0.0.1:8088 \
  VIEWER_SELECTOR='#screen' \
  VIEWER_SETUP_CLICKS='["#toolbar-toggle","[data-action=control]"]' \
  bun scripts/bench/run.ts content-screen-relay-text :93 :94 ENGINE_HOST_PID \
    "$HOME/bench-results/content" --repeat 3

# Start the native Moonlight viewer separately, as in the baseline setup.
# Sunshine's video port is base port + 9; change this for your configuration.
BENCH_QUALITY=1 BENCH_WORKLOAD=text BENCH_UDP_PORT=49009 \
  bun scripts/bench/run.ts content-moonlight-text :96 :95 SUNSHINE_PID \
    "$HOME/bench-results/content" --repeat 3
```

Repeat for `drag` and `gradient`. Run products sequentially, closing native
streaming clients before measuring another product. Quality mode has no input
trials: it measures a 20-second motion phase after four seconds settling and one
second idle, followed by a separate ten-second matched-frame phase. Performance
and host CPU/RSS do **not** include full-frame capture, PPM writes or FFmpeg work.
Ordinary input benchmarks are unchanged. Quality validity requires >=10 matched
pairs, a nonzero measured bitrate and the usual video/polling validity gates.

Both browser viewports must be 1280x720 at device scale 1. Without a window
manager, Chromium's fullscreen API alone does not resize the X window. Quality
mode compensates the outer window's browser-chrome height and moves that chrome
above the Xvfb screen. The visible canvas/video is native-size; the paired reader
rejects scaled viewers (allowing two pixels of color-guard edge uncertainty).
Native Moonlight must likewise display the source 1:1.

`frames.c` attaches persistent XShm readers to **both** X servers. Full-frame
reads are verified by a second marker decode, so changing frames are discarded.
Eight source frames sampled every 15 IDs are retained; a client frame is saved
only when its checksummed ID matches a retained source. Source/client XShm images
are independently captured, not reconstructed from canvas commands. These are
**dummy-data-only benchmark displays**: XShm segments use mode 0666 so an X server
running as root inside the host-IPC Docker container can attach. Segments are
marked for deletion immediately after attachment. Do not use sensitive desktops.

The shared ROI is 1120x360, starting 120 pixels below the marker top and at its
first data cell; marker, browser UI and toolbars are excluded. FFmpeg converts
both RGB PPMs to gray luma, computes 8x8 SSIM and peak-255 PSNR on identical
geometry, and saves a 640x180 text crop from the middle matched pair. JSON stores
every pair's source ID, paths, scale, SSIM and PSNR, their distributions and the
lossless-pair count (`inf` PSNR is explicit, not fabricated as a finite score).
`PRODUCT.quality-text-{source,client}.png` are legibility evidence; all paired PPMs
are retained. Repeat summaries use the median of per-run quality medians and
min–max spread, not pooled frames or confidence intervals.

Received bitrate scopes are deliberately explicit and are **not interchangeable**:

- Nanoctl: browser inbound-video RTP `bytesReceived` counter deltas, excluding
  transport headers.
- Screen-relay: CDP Network received WebSocket payload bytes, decoding binary
  frames from base64, including protocol/control messages but not WS/TCP headers.
- Sunshine: IPv4 video-port UDP payload lengths from a header-only Ethernet PCAP
  on loopback (`tcpdump -Q in`, so loopback traffic is counted once), including
  Sunshine packet headers/FEC but excluding UDP/IP headers. This requires
  passwordless `sudo -n tcpdump` and `sudo -n kill`; capture stops at motion end,
  and nonzero kernel drops fail the run. Raw PCAP and tcpdump counters are kept.

The source is generated canvas content, not a real editor/window manager or
prerecorded video. Luma scores exclude chroma fidelity and are limited to this
ROI. The paired phase follows the timing phase to avoid observer overhead, so
quality and throughput are not computed from the same individual frames.

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
