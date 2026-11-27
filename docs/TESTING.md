# Testing

## Local checks

Install Bun and a Rust toolchain, then:

```sh
bun install --frozen-lockfile
bun run check
bun run test:browser
NANOCTL_BROWSER_MEDIA=1 bun run test:browser
cargo fmt --manifest-path crates/nanoctl-agent/Cargo.toml -- --check
cargo clippy --all-targets --features media --manifest-path crates/nanoctl-agent/Cargo.toml -- -D warnings
cargo test --features media --manifest-path crates/nanoctl-agent/Cargo.toml
for script in install.sh packaging/tailnet.sh packaging/linux/*.sh packaging/macos/*.sh; do
  sh -n "$script"
done
shellcheck -x install.sh packaging/tailnet.sh packaging/linux/*.sh packaging/macos/*.sh
```

`check` runs formatting, lint, typechecks, Bun tests and Rust RTC tests.
`test:browser` builds the viewer and starts the real Rust agent with the RTC feature,
a private test config, free loopback port, TLS off and explicit dev identity.
Chromium checks CSP/absence of remote runtime requests, host name/document title,
identity, offer/answer, replacement, reconnection and rejected foreign Origin.
Media mode additionally checks first video frame and a nonzero fractional bitrate.
Unit tests cover rate baselines, missing browser FPS, counter resets and idle samples.

Media builds need native capture/encoder development dependencies. On Linux this
includes X11/XCB, PipeWire/Wayland, libva, NVENC headers, clang and pkg-config; CI lists the full
platform packages. Media browser tests need a usable graphical display, e.g. `DISPLAY=:0`.
Install Chromium with `bunx playwright install chromium` if absent.
Test code's `NANOCTL_DEV_LOOPBACK_USER=dev@example.com` bypass is never a production setting.

## End-to-end latency benchmark

On Linux, install Xvfb and xauth in addition to the media build dependencies and
Playwright Chromium, then run:

```sh
xvfb-run -a -s "-screen 0 1280x720x24 -nolisten tcp" bun run test:latency
```

On Windows or macOS, run `bun run test:latency` in an unlocked interactive session
with a single display (preferably 1280×720) and no other window stealing focus.
On macOS the agent must have Screen Recording and Accessibility permissions;
hosted macOS runners cannot be assumed to have those grants, so run it on an
authorized local desktop. Windows is experimental in hosted CI because the
runner may not have usable desktop capture or input. The Linux CI job must
produce results. Do not run on a desktop with sensitive content.

A dedicated Chromium window paints a checksummed frame counter and changes the
encoded input counter only when the OS delivers a real Space keydown. The release
media agent captures that desktop, encodes software H.264 at up to 1280×720 and
30 fps, and serves its real viewer to headless Chromium over loopback WebRTC
using the existing browser-test identity. Playwright presses Space in the viewer,
which sends it through the normal input data channel and agent injection path.
The probe decodes counters from the received video, after startup warm-up.

- **Video latency:** Time from drawing a unique source marker to the viewer's
  estimated presentation time (`requestVideoFrameCallback.expectedDisplayTime`).
  Both browser clocks are mapped to the runner's monotonic clock using the
  midpoint of the shortest of ten local DevTools round trips per browser. The
  sum of the two half-round-trip times is reported as alignment uncertainty.
- **Input-to-photon proxy:** Time from the viewer keydown timestamp to estimated
  presentation of the first decoded frame with the corresponding input counter.
  This measures 12 sequential presses on the viewer's clock alone.
- **Presented fps:** Change in Chromium's presented-frame counter divided by
  elapsed presentation time during the measurement.

The benchmark writes `latency.json` (including raw milliseconds) and `latency.md`
under `test-results/`. CI publishes them as artifacts and shows nearest-rank
p50/p95/p99/max and fps in the job summary. Missing markers or input responses
fail the benchmark; there is no timing regression threshold yet. Compare runs
with comparable runner, display and encoder settings. This is a glass-to-glass
**software proxy**, not an optical measurement: it excludes monitor scanout,
input hardware delay and real tailnet network conditions. It does not measure
hardware encoders.

## Build and embed

```sh
bun run build:agent
# Or, for a debug binary:
bun run build:viewer
cargo build --features media --manifest-path crates/nanoctl-agent/Cargo.toml
```

Verify that the binary serves the built hashed viewer assets, not the no-viewer placeholder.
Cargo must also compile without Bun or viewer output. The viewer cannot depend on remote
runtime origins or unsafe-eval.

## Encode-only performance

Run the ignored release test to measure RGBA-to-Annex-B latency without desktop
capture, scheduling or WebRTC. Set `NANOCTL_BENCH_ENCODER` to `software-openh264`
(default), `vaapi`, `nvenc`, `videotoolbox` or `media-foundation`. A requested hardware
backend must initialize and encode; the benchmark will not substitute software.

```sh
test_bin=$(cargo test --release --features media --manifest-path crates/nanoctl-agent/Cargo.toml \
  --bin nanoctl --no-run --message-format=json |
  jq -r 'select(.profile.test == true and .target.name == "nanoctl" and .executable) | .executable')
NANOCTL_BENCH_ENCODER=software-openh264 /usr/bin/time -p "$test_bin" \
  media::tests::benchmark_encoder --exact --ignored --nocapture
```

The test reports p50/p95 wall time and output count for 180 unpaced, synthetic
1080p frames after warmup, configured at 30 fps and 4 Mbps. On Unix, `time -p`
reports process CPU time (user + sys, including warmup and frame generation);
use `Measure-Command` for wall time or a process profiler for CPU time on Windows.
Compare encoders on the same machine and report the GPU and driver alongside the result.

## Release qualification

Local fake-Tailnet tests cover status/whois parsing, owner/users/tags/grants policy,
Host/Origin checks, TLS paths/renewal and real WS offer/answer. Packaging fixtures cover
Unix service rollback, config preservation, unavailable Tailscale/URL and best-effort
credential deletion; Windows source assertions do not substitute for Windows execution.

Before release, exercise real Tailscale HTTPS issuance/renewal, HTTP fallback, owner,
view-only and tagged peers, grants, UDP paths and DERP underlay. On each native OS,
test fresh install, v2 upgrade, failed-start rollback, ready markers, interactive-user
permissions, desktop switching and hardware/software encoding. Inspect the live viewer
and stats after multiple samples, not just the first frame.

`NEXT_VERSION.md` and `research/next-version-reliability.md` remain historical mixed
native/reliability notes, not the 3.0 architecture or evidence of passing release gates.

## v4 bad-network gate

On Linux, install the browser test dependencies, Xvfb, and `iproute2`. Build the
media agent before entering the isolated network namespace.

```sh
bun run build:agent
scripts/v4-bad-network.sh crates/nanoctl-agent/target/release/nanoctl
```

The test applies 5% random UDP packet loss, 25% reordering, 25 ms delay with
8 ms jitter, and a shared 3 Mbps cap to the real WebRTC traffic. Signaling uses
TCP and stays outside the fault profile. The viewer must present new desktop
content for 60 seconds with gaps no longer than 500 ms, then pass 20 reattach
cycles under the same profile. The gate also checks peak and sustained
keyframe rates, host RSS growth, and queue limits. The run writes the profile,
observed counters, and timestamps to `test-results/*/bad-network.json`.

The draft v4 network trial runs the same gate a second time with the host SCTP
minimum retransmission timeout set to 200 ms. That job patches a temporary copy
of the locked dependency and uploads a separate artifact. The stock build still
runs the full gate.
