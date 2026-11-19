# Loopback media benchmark (Linux)

Build with `cargo build --release --features media --manifest-path crates/nanoctl-agent/Cargo.toml`.
Run `DISPLAY=:0 NANOCTL_DEV_LOOPBACK_USER=bench RUST_LOG=error nanoctl media-bench --seconds 30 --json`.
Use `--log-file PATH` to keep diagnostic logs separate from JSON.

The hidden command connects a real production `HostPeer` to an in-process webrtc-rs
controller with default RTCP report/NACK interceptors. Controller ICE candidates
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
