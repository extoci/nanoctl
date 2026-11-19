# Initial cross-product run

Ubuntu 22.04 VM, no GPU, loopback, 1280x720 host/client X displays,
software H.264, default 60-fps ceilings. Same source application, seed 1729,
120 randomized Space trials. Products ran sequentially. These are software
draw/injection-to-client-pixels proxies, not optical input-to-photon results.

**Incomplete: nanoctl input trials are invalid (0/120 responses).** Its video,
resource and cadence observations below are diagnostic only; do not interpret
this table as a valid end-to-end latency ranking. Moonlight passed all 120 trials.

| Product                 | Inputs  | Input p50/p95/p99 ms  | Video p50/p95/p99 ms  | Unique fps | Frame interval p50/p95/p99 ms | Stalls >100ms | Host CPU idle/motion % | Host RSS idle/motion MiB | Host runtime MiB |
| ----------------------- | ------- | --------------------- | --------------------- | ---------- | ----------------------------- | ------------- | ---------------------- | ------------------------ | ---------------- |
| nanoctl (INVALID input) | 0/120   | n/a                   | 37.58 / 40.23 / 53.03 | 57.37      | 16.01 / 24.03 / 36.01         | 0             | 0.88 / 117.79          | 134.43 / 135.26          | 15.40            |
| Sunshine/Moonlight      | 120/120 | 28.05 / 40.44 / 58.18 | 16.44 / 24.51 / 38.20 | 58.62      | 16.19 / 24.00 / 33.68         | 0             | 45.35 / 46.27          | 260.29 / 269.81          | 28.94            |

Host CPU uses one core = 100%. Both phases have 40 idle samples (~10.25 s)
and 236 motion samples (~59.18 s), sampled every 250 ms. RSS sums the host
process tree, not the source browser or client. Runtime sizes are logical bytes
of nanoctl's embedded-viewer release binary, versus Sunshine binary plus
`/usr/share/sunshine`; system libraries, Moonlight/Chrome clients and caches are
excluded. They are not full dependency-inclusive deployment sizes.

| Detector  | Polls | Decodable polls | Unique video samples | Poll rate | Poll interval p99 ms | Read p99 ms | Reader CPU | Clock uncertainty |
| --------- | ----- | --------------- | -------------------- | --------- | -------------------- | ----------- | ---------- | ----------------- |
| nanoctl   | 14727 | 14727           | 3322                 | 249.59 Hz | 4.65                 | 0.47        | 1.58%      | ±0.34 ms          |
| Moonlight | 14694 | 14694           | 3400                 | 249.05 Hz | 6.08                 | 1.87        | 1.63%      | ±0.33 ms          |

The ~4 ms polling quantization is significant for these latencies. Tail polling
gaps exceed 4 ms, particularly for Moonlight. Reader CPU excludes the client X
server's work servicing XShm. Source canvas draw timestamps are software draw
submission times. Native source and detector clocks both use CLOCK_MONOTONIC.

Nanoctl default ceiling: 24 Mbps. Received video RTP bitrate from real headed
Chrome getStats: **0.198 Mbps** averaged across the motion snapshots. Its log
reported `send_kbps=196`, `target_kbps=24000`, `target_fps=60`, encoded 1280x720
near the end of the run. This simple marker workload is not a quality test.
Moonlight requested its default **10000 kbps** at 720p/60; Sunshine logged
`Streaming bitrate is 7308000` for the active software H.264 stream. That is a
configured encoder target, not a measured received bitrate.

## Input blocker and setup corrections

Nanoctl source input remained zero in three full trial runs, including the run
with explicitly focused, named host and client X windows and the video element
focused. No valid input latency can be computed. The same helper and source
application subsequently produced 120 trusted Space events through Moonlight,
disproving a universally broken XTest/source implementation. The remaining
nanoctl client-event/data-channel/host-injection link is **unverified**: do not
call this a confirmed nanoctl product defect. The runner now records source
key events in the source JSON and browser-viewer keys in result JSON so the next
diagnostic run can localize that link before another full benchmark.

Earlier setup attempts exposed two harness issues, corrected before the above
measurements: source/client chrome causes letterboxing, so marker rows must be
located rather than assuming y=120; Bun's `process.hrtime` has a process-relative
origin here, so use libc `clock_gettime` for native clock alignment. Selecting
browser windows by explicit title avoids focusing a larger blank startup window.

Raw outputs on this VM: `~/bench-results/{nanoctl,moonlight}.{json,md,pixels.log,source.json}`.
Visual evidence of the changed source through the clients:
`~/nano-motion-client5.png`, `~/moonlight-motion.png`. Screenshots were taken
during startup/early motion; they do not establish successful nanoctl input.
Servers and externally started Moonlight were left running; runner-owned source
and browser viewer windows close at completion.
