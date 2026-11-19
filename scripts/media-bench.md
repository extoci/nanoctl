# Loopback media benchmark (Linux)

Build with `cargo build --release --features media --manifest-path crates/nanoctl-agent/Cargo.toml`.
Run `DISPLAY=:0 NANOCTL_DEV_LOOPBACK_USER=bench RUST_LOG=error nanoctl media-bench --seconds 30 --json`.
Use `--log-file PATH` to keep diagnostic logs separate from JSON.

The hidden command connects a real production `HostPeer` to an in-process webrtc-rs
controller with default RTCP report/NACK interceptors. Both RTP and receiver RTCP
are drained; reading RTCP is required for webrtc-rs to process incoming sender
reports and populate RR LSR/DLSR. JSON includes received/matching sender-report
counts and last encoded dimensions. Controller ICE candidates
are restricted to loopback. No synthetic loss reports are injected. REMB
emulation is opt-in with `NANOCTL_BENCH_CHROME_REMB=1`.
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

On Linux X11, `damage_enabled` reports XDamage gating and `capture_parked` reports
its final state. `damage_resumes` includes nonvisual repaints; identical pixels
return to parked immediately. `first_change_samples` counts successful changed
encodes after parking. `first_change_last_ms` / `first_change_max_ms` measure from
the local receipt of DamageNotify to completion of that encode, including capture
and pacing. They are session-wide gauges (including warmup), not network/display
latency. Other event/counter fields are measurement-window deltas.
`first_change_capture_ready_ms` isolates the last event→captured-frame-ready part;
`first_change_encode_ms` is that frame's color conversion and encoder duration.

The X11 gate uses xcap's one-shot capture primitive rather than its continuous
recorder: in xcap 0.9.7 `stop()` can race with the worker's running check and end
the worker permanently, so stop/start cannot reliably resume it. Gating before
creating that worker avoids both the race and an abandoned sleeping worker. The
500 ms active grace is based on actual pixel changes, not redundant repaint
notifications. Missing XDamage or Wayland retains the continuous recorder;
runtime damage-connection errors revert to continuous one-shot captures. Cached
keepalive and forced-keyframe encoding do not interact with the capture gate.

The direct Linux dependency is xcb 1.7 (MIT), already used by xcap; its `damage`
feature also enables xfixes. libc (MIT OR Apache-2.0) supplies descriptor polling.

## Application-limited REMB and recovery

`bash scripts/media-remb-bench.sh BINARY OUTPUT_DIR PREFIX [SCENARIOS...]` runs
idle, motion, throttled (30 seconds each), idle-motion (10 seconds static then
20 seconds motion), and recovery (20 seconds throttled then 30 seconds clean).
It uses the same scene, quality, warmup and shaping parameters as above. A
`NANOCTL_BENCH_READY_FILE` marker synchronizes transitions to measurement start;
the runner polls it every 50 ms. Phase timing is therefore nominal to 50 ms,
plus scene startup. Cleanup also stops the agent on interruption.

The emulated controller sends real REMB RTCP once per second for the video SSRC:
`1.5 * incoming_kbps + 10` over a rolling two-second payload window, or
`0.85 * incoming_kbps` when sender-report one-way delay rises by over 5 ms.
Sender/receiver share the system clock, so SR NTP arrival delay is measurable.
`incoming_delay_ms` and `feedback.remb_kbps` expose the emulation and delivery.
This is a regression stress model, not a full implementation of Chrome AIMD:
upstream AIMD uses the 1.5x+10 expression as an increase ceiling, not an
unconditional idle-time decrease. No actual browser runs in this harness.

`send_kbps` measures successful writer payloads over two seconds. New REMB values
bind only below 95% of that rate; an old value carried by later Receiver Reports
cannot recap a new probe. Idle damage pacing freezes the target/FPS; the first two
seconds of resumed motion ignore stale idle REMB while loss/RTT remain active.
After three clean measured seconds, recovery probes multiplicatively with an
RTT-weighted gain (up to 1.5x/s on low RTT); congestion memory expires after ten
clean seconds. Resolution hysteresis restores full size at 1800 kbps.
Targets, FPS and sizes are absolute gauges, not measurement-window deltas.
Time to target >=4000 kbps can be read from `target_series` at one-second
resolution, relative to second 10 (idle-motion) or 20 (recovery). Idle never
needs to fill the link; its keepalive gaps are intentional.
