# Loopback media benchmark (Linux)

Build with `cargo build --release --features media --manifest-path crates/nanoctl-agent/Cargo.toml`.
Run `DISPLAY=:0 NANOCTL_DEV_LOOPBACK_USER=bench RUST_LOG=error nanoctl media-bench --seconds 30 --json`.
Use `--log-file PATH` to keep diagnostic logs separate from JSON.

The hidden command connects a real production `HostPeer` to an in-process webrtc-rs
controller with default RTCP report/NACK interceptors. Both RTP and receiver RTCP
are drained; reading RTCP is required for webrtc-rs to process incoming sender
reports and populate RR LSR/DLSR. JSON includes received/matching sender-report
counts and last encoded dimensions. Controller ICE candidates
are restricted to loopback. No synthetic loss reports or REMB are injected.
Capture/encode/send uses the real `spawn_video` pipeline, OpenH264, an 8000 kbps
ceiling, 60 fps and a 1920x1080 size limit. Software is selected explicitly so both
revisions use the same encoder on machines without a render device.

After connecting, five seconds of pipeline warmup are excluded. CPU is the process
`/proc/self/stat` utime+stime delta divided by `getconf CLK_TCK` and measured wall
time: 100% means one core. It includes the in-process controller's work, but not
the separate scene-generator process. Counter values are measurement-window
deltas; target values and per-second snapshots are absolute. Receiver bitrate is
RTP payload only, excluding transport headers. Frames are unique RTP timestamps
whose end marker was received, **not decoded/validated complete H.264 pictures**.
Intervals are consecutive marker arrivals, using nearest-rank percentiles; idle
keepalive gaps are intentional. Setup/first-frame latency is not measured.

`bash scripts/media-bench.sh BEFORE_BINARY AFTER_BINARY OUTPUT_DIR [SECONDS]`
runs six measurements (at least 30 seconds each). Requires `ffplay`, `xrandr`,
`python3`, `rg`, `tc` and passwordless sudo. It changes the display to 1920x1080,
uses a fullscreen constant-color idle scene or `testsrc2` motion at 60 fps, and
restores the original mode. The throttled scenario applies
`sudo -n tc qdisc add dev lo root netem rate 2mbit delay 40ms loss 2%` separately
for each revision, avoiding carry-over queue backlog. EXIT/INT/TERM cleanup
removes the owned qdisc and stops the scene; an existing root qdisc is refused.
The shaping affects all loopback traffic while active. Do not run builds, tests,
or unrelated workloads concurrently. One run per cell is exploratory evidence,
not a statistically controlled performance claim.

Stage counters expose `fresh_frames` consumed by the producer, `pacer_skips`,
`encode_attempts`, `empty_encodes`, and cumulative wall nanoseconds in
`capture_wait_ns`, `compare_ns`, `preprocess_ns` (view/resize), and `backend_ns`
(color conversion plus encoder). `color_convert_ns` isolates software RGBA→I420
conversion within `backend_ns`. Measurement-window counters are deltas; the
per-second series is cumulative. Timings are wall time, not thread CPU.
`thread_cpu_percent` samples `/proc/self/task/*/stat` across the measurement;
`pipeline.producer_tid` identifies the blocking producer, and `nanoctl-capture`
is the forwarding thread. Other thread names include xcap and Tokio workers.
Threads that exit during the window are absent from the final thread snapshot;
whole-process CPU still counts them. Exact unchanged-frame comparison is retained:
sampling or hashing cannot eliminate that work without a full equality check or
a risk of missing small changes. Capture cadence is not reduced while idle.
`changed_encodes` / `unchanged_encodes` classify successful encoder outputs by
exact captured RGBA equality with the preceding successful output. These are
source-pixel classifications, not decoded-picture validation. Raw received-marker
FPS includes redundant images in the fixed-timer baseline, so it is not by itself
a measure of unique motion updates.
