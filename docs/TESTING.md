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
