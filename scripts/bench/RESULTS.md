# screen-relay Docker versus current nanoctl, five runs each

Measured sequentially on 2026-11-19: screen-relay 0.1.2 x5, then the current
nanoctl release (retained half-frame pacer and teardown trimming) x5. Both use the
same VM, Chromium 151.0.7922.34 source/client, 1280x720 displays, software H.264,
default 60-fps ceiling and otherwise default streaming settings. Every run
matched 120/120 trusted XTest inputs: **5/5 valid runs and 600/600 inputs per
product**. No outlier was removed. Cells below are **median of per-run metrics
[minimum–maximum]**, not pooled percentiles or confidence intervals.

| Product      | Input p50 ms        | Input p95 ms         | Input p99 ms         | Video p50 ms        | Video p95 ms        | Video p99 ms        |
| ------------ | ------------------- | -------------------- | -------------------- | ------------------- | ------------------- | ------------------- |
| screen-relay | 64.23 [56.05–96.09] | 72.25 [72.22–108.05] | 76.05 [73.83–108.46] | 55.06 [46.27–88.15] | 57.48 [57.39–91.03] | 62.47 [61.75–98.10] |
| nanoctl      | 32.16 [31.94–40.37] | 40.33 [36.42–52.38]  | 44.23 [40.40–64.04]  | 21.09 [19.94–35.77] | 37.77 [22.31–38.88] | 38.79 [27.28–43.72] |

| Product      | Unique FPS          | Frame interval p50 ms | Frame interval p95 ms | Frame interval p99 ms | Stalls >100 ms |
| ------------ | ------------------- | --------------------- | --------------------- | --------------------- | -------------- |
| screen-relay | 59.74 [59.45–59.89] | 16.01 [16.01–16.02]   | 20.26 [20.15–20.34]   | 28.00 [24.19–28.18]   | 0 [0–0]        |
| nanoctl      | 59.96 [59.85–59.99] | 16.03 [16.03–16.04]   | 20.23 [20.20–20.25]   | 23.97 [20.45–24.30]   | 0 [0–0]        |

| Product      | Idle CPU %       | Motion CPU %        | Idle RSS MiB        | Motion RSS MiB      | Runtime MiB            |
| ------------ | ---------------- | ------------------- | ------------------- | ------------------- | ---------------------- |
| screen-relay | 0.88 [0.49–0.99] | 26.53 [25.97–27.73] | 48.27 [48.27–48.27] | 48.27 [48.27–48.27] | 139.59 [139.59–139.59] |
| nanoctl      | 0.60 [0.50–0.88] | 36.15 [33.55–36.79] | 49.55 [43.53–57.63] | 56.57 [53.45–61.97] | 23.46 [23.46–23.46]    |

Nanoctl is faster and smaller in this workload; screen-relay uses less motion
CPU and engine RSS. Screen-relay's between-run latency spread is substantial.
This marker workload does not establish general picture quality or throughput
at comparable visual quality. No product tuning or product-code changes were
made for this comparison.

## Working setup and scope

The upstream Docker page was checked. `screen-relay.Dockerfile` uses Ubuntu
24.04 plus Node 24 (resolved version 24.22.3), installs the pinned npm package,
then runs `screen-relay install --backend x11 --desktop none --yes` without
patches. Built image identity:
`sha256:e353b3230bd8090877d11f1fbe47ccfb6be0fefc6c48c2c83e58d99d5dee5a6c`.
The full build image is 1,163,872,443 bytes, including its OS and development
tools; that is **not** the runtime figure in the table.

Container `nanoctl-screen-relay` uses host networking and host IPC, sharing
`/tmp/.X11-unix`. Xvfb :93 is inside the container; the identical VM source
Chromium connects to it. Headed viewer Chromium and the persistent detector use
VM display :94. The service binds only `127.0.0.1:8088`, with no remote auth token
required. See README for the exact build, startup, PID selection and run commands.
Nanoctl uses source :97, the same client :94, localhost :7338 and the existing
explicit dev-loopback identity. Its agent was restarted before its five runs.

The screen-relay viewer is **WebCodecs -> canvas#screen**, not WebRTC video. It
starts view-only. `VIEWER_SETUP_CLICKS` opens the toolbar and clicks the actual
Take control button; both viewers then use the existing surface-focus/center-click
setup before XTest trials. A separate post-measurement check verified a trusted
Space targeted `#screen`, a trusted source Space incremented the counter, and the
decoded viewer marker reached input 1. Visual evidence:
`~/bench-candidates/screen-relay-input-verified.png` (visible counter 1 and
"you have control"). An initial diagnostic used a fixed marker row and timed
out because browser chrome shifts the marker; scanning rows fixed that check.
The benchmark already discovers the marker row and required no such change.

Host process-tree sampling rooted at host PID **137681** counts the screen-relay
engine and descendants, **not** parent Node supervisor, container init or Xvfb.
One CPU core is 100%. `/metrics` cgroup figures were not used. Runtime snapshots
include the **npm package + bundled viewer/JS dependencies/sources**, built
engine, **Node runtime**, listed X11/CLI helper executables and their transitive
ELF shared-library closure. Nanoctl includes its release binary and its own ELF
library closure. Logical runtime bytes: screen-relay **146,374,952**; nanoctl
**24,603,072**. Both exclude other base-OS utilities, Xvfb, source/viewer browsers,
development toolchain, headers/static build archives, caches and Docker layers.
Older nanoctl app-only sizes (~15.42 MiB) excluded shared libraries and therefore
are not directly comparable with this expanded counting scope.

Screen-relay bitrate defaults are **1/8/40 Mbps min/start/max**; a run-five
`/metrics` sample reported a **40,000,000-bps encoder target**. That is not
measured payload throughput. Nanoctl's default ceiling is 24 Mbps, with measured
received video RTP **0.203 [0.202–0.203] Mbps** across these sparse-marker runs.
No equivalent WebSocket throughput measurement was collected for screen-relay;
do not infer one from its target or compare that target to nanoctl's RTP rate.

## Detector and artifacts

| Product      | Poll Hz                | Poll interval p99 ms | Capture p99 ms   | Reader CPU %     | Clock uncertainty ±ms |
| ------------ | ---------------------- | -------------------- | ---------------- | ---------------- | --------------------- |
| screen-relay | 249.35 [248.76–249.38] | 4.66 [4.62–5.42]     | 0.52 [0.50–0.53] | 1.35 [1.33–1.42] | 0.23 [0.21–0.29]      |
| nanoctl      | 249.43 [249.16–249.76] | 4.63 [4.47–4.67]     | 0.59 [0.54–0.63] | 1.60 [1.53–1.65] | 0.27 [0.23–0.52]      |

The roughly 4-ms polling quantization remains significant. Reader CPU excludes
the client X server's capture-serving work. Total screen-relay/nanoctl detector
polls: **73,527 / 73,588**; joined video observations: **17,317 / 17,384**.
Each product has 200 idle resource samples; motion samples are 1,276 / 1,281.
These remain software input-to-client-pixels measurements, not optical photon
measurements or a network/physical-display benchmark.

Raw JSON, source traces, pixel logs, per-run Markdown and repeat summaries are
preserved as `~/bench-results/docker-{screen-relay,nanoctl}*`. Runtime snapshots
and adjacent manifests: `~/bench-runtime/{screen-relay,nanoctl}{,.json}`. Build,
agent and run logs: `~/bench-candidates/`. Native Rust code was unchanged.

# Cross-product runs

Ubuntu 22.04 VM, no GPU, loopback, 1280x720 host/client X displays,
software H.264, default 60-fps ceilings. Same source application, seed 1729,
120 randomized Space trials. Products ran sequentially. These are software
draw/injection-to-client-pixels proxies, not optical input-to-photon results.

Both products passed all 120 input trials. Nanoctl was rerun after fixing the
viewer focus bug described below; Moonlight's numbers are from the earlier run.
The benchmark setup and measurement code were unchanged for the nanoctl rerun.
These single runs are preliminary observations, not repeated-run confidence bounds.

| Product            | Inputs  | Input p50/p95/p99 ms  | Video p50/p95/p99 ms  | Unique fps | Frame interval p50/p95/p99 ms | Stalls >100ms | Host CPU idle/motion % | Host RSS idle/motion MiB | Host runtime MiB |
| ------------------ | ------- | --------------------- | --------------------- | ---------- | ----------------------------- | ------------- | ---------------------- | ------------------------ | ---------------- |
| nanoctl            | 120/120 | 60.04 / 76.53 / 90.81 | 53.49 / 57.24 / 72.20 | 55.83      | 16.03 / 32.00 / 36.01         | 0             | 0.88 / 113.33          | 57.59 / 61.91            | 15.40            |
| Sunshine/Moonlight | 120/120 | 28.05 / 40.44 / 58.18 | 16.44 / 24.51 / 38.20 | 58.62      | 16.19 / 24.00 / 33.68         | 0             | 45.35 / 46.27          | 260.29 / 269.81          | 28.94            |

Host CPU uses one core = 100%. Each run has 40 idle samples (~10.25 s).
Nanoctl has 235 motion samples (~58.94 s); Moonlight has 236 (~59.18 s),
sampled every 250 ms. RSS sums the host
process tree, not the source browser or client. Runtime sizes are logical bytes
of nanoctl's embedded-viewer release binary, versus Sunshine binary plus
`/usr/share/sunshine`; system libraries, Moonlight/Chrome clients and caches are
excluded. They are not full dependency-inclusive deployment sizes.
Nanoctl's host was freshly restarted for this rerun; its lower RSS versus the
previous invalid run must not be attributed to the focus fix.

| Detector  | Polls | Decodable polls | Unique video samples | Poll rate | Poll interval p99 ms | Read p99 ms | Reader CPU | Clock uncertainty |
| --------- | ----- | --------------- | -------------------- | --------- | -------------------- | ----------- | ---------- | ----------------- |
| nanoctl   | 14688 | 14688           | 3213                 | 248.95 Hz | 6.72                 | 1.61        | 1.54%      | ±0.37 ms          |
| Moonlight | 14694 | 14694           | 3400                 | 249.05 Hz | 6.08                 | 1.87        | 1.63%      | ±0.33 ms          |

The ~4 ms polling quantization is significant for these latencies. Tail polling
gaps exceed 4 ms for both products. Reader CPU excludes the client X
server's work servicing XShm. Source canvas draw timestamps are software draw
submission times. Native source and detector clocks both use CLOCK_MONOTONIC.

Nanoctl default ceiling: 24 Mbps. Received video RTP bitrate from real headed
Chrome getStats: **0.198 Mbps** averaged across the motion snapshots. Its log
reported `send_kbps=198`, `target_kbps=22348`, `target_fps=60`, encoded 1280x720
near the end of the run. This simple marker workload is not a quality test.
Moonlight requested its default **10000 kbps** at 720p/60; Sunshine logged
`Streaming bitrate is 7308000` for the active software H.264 stream. That is a
configured encoder target, not a measured received bitrate.

## Input bug: normal pointer capture loss blurred the viewer

The initial nanoctl run had 0/120 responses. Temporary CDP capture-phase DOM
listeners and a temporary `RTCDataChannel.send` wrapper established this chain:

1. The harness's real XTest mouse click reached the video. The viewer's
   `onPointer` focused the video, called `setPointerCapture`, and sent pointer
   down/up control messages with input enabled.
2. Chrome implicitly released capture after pointerup, dispatching
   `lostpointercapture`. The viewer incorrectly routed this normal event to
   `onPointerCancel`, which called `releaseInput`: a release message was sent,
   then `video.blur()` ran. The CDP trace records capture loss followed by
   release and focusout.
3. Subsequent XTest Space events arrived as trusted DOM keydown/keyup with
   `code="Space"`, but targeted BODY. The video-only `onKey` handler never ran;
   no key data-channel messages or host counter changes occurred. Input was on
   and the reliable control channel remained open (pings still traversed it).
4. As a countercheck, explicitly refocusing the video without changing the
   channel, permissions, key mapping or host immediately produced Space down/up
   messages and a trusted host counter increment. The existing #13 harness
   focuses the video before Playwright key presses, without a subsequent click.

This was a real viewer bug, not an XTest or benchmark setup defect. The viewer
now handles normal capture loss (`buttons === 0`) without releasing keyboard
focus or held keys. Unexpected capture loss with buttons held and pointer
cancellation still release input and blur; input-off behavior remains unchanged.

The post-fix CDP trace shows a native click followed by trusted Space on VIDEO,
Space down/up on `nanoctl.control.v1`, and host input counter 1 before any manual
refocus. Agent-side existing Enigo debug logging confirms the injection:

```text
key(key: Space, direction: Press)
xtest_fake_input with keycode 65, deviceid 3, delay 0
entered the key via x11
key(key: Space, direction: Release)
```

Diagnostic evidence on this VM:
`~/bench-results/nanoctl-{before,after}-input-trace.txt` and
`~/nanoctl-input-fixed-trace.log`. Temporary CDP hooks were removed, and Enigo
debug logging disabled, before the full measured run. That run recorded 120
trusted viewer Space events, 120 trusted source Space events, 120 matching pixel
responses, no missing inputs, and `valid: true`. Unit coverage exercises normal
capture loss, unexpected loss, cancellation, input off and a closed channel;
browser coverage checks repeated clicks retain focus and forward Space, plus
the existing disabled-input regression.

## Earlier setup corrections

Earlier setup attempts exposed two harness issues, corrected before the above
measurements: source/client chrome causes letterboxing, so marker rows must be
located rather than assuming y=120; Bun's `process.hrtime` has a process-relative
origin here, so use libc `clock_gettime` for native clock alignment. Selecting
browser windows by explicit title avoids focusing a larger blank startup window.

Raw outputs on this VM: `~/bench-results/{nanoctl,moonlight}.{json,md,pixels.log,source.json}`.
The original invalid nanoctl artifacts were preserved as `nanoctl-before-fix.*`.
Visual evidence of the changed source through the clients:
`~/nano-motion-client5.png`, `~/moonlight-motion.png`. Screenshots were taken
during startup/early motion; they do not establish successful nanoctl input.
Servers and externally started Moonlight were left running; runner-owned source
and browser viewer windows close at completion.

## Profile and optimization runs

The same harness, displays, workload, software encoder, 60-fps ceiling and
120-trial seed were retained. Each accepted change was built in release mode,
the agent was restarted, and products ran sequentially without concurrent
builds/tests. The profiling baseline is a fresh run, not the earlier focus-fix
run. These are single-run observations: notably, baseline video p50 varies by
approximately one display refresh between runs. Do not attribute every small
row-to-row change to the patch.

### Agent stage timings

`PipelineStats` logs cumulative nanoseconds and operation counts every ten
seconds. The following are deltas between the second and final log records,
after the idle phase. Capture time is divided by successful captures;
conversion/backend time by encode attempts; send time by sample writes.
OpenH264-only time is backend time minus color conversion, not another timer.
These are elapsed-time means, not CPU samples or a distribution. Capture and
encoding run on separate workers, so the columns must not be summed into an
end-to-end latency estimate.

| Stage, mean ms/operation              | Profile baseline | Persistent capture | Final, playout delay |
| ------------------------------------- | ---------------: | -----------------: | -------------------: |
| X11 capture                           |            5.120 |              2.168 |                2.003 |
| Frame comparison, per capture         |            0.131 |              0.115 |                0.102 |
| Resize/preprocess, per encode attempt |           0.0003 |             0.0007 |               0.0003 |
| RGBA to I420                          |            1.598 |              1.522 |                1.394 |
| OpenH264/backend excluding conversion |            3.996 |              3.702 |                3.445 |
| Packetize/send sample                 |            0.061 |              0.058 |                0.055 |

The baseline window captured **8,802** frames for **3,145** encode attempts
(2.80 captures/attempt). Final: **3,229** captures for **3,197** attempts
(1.01 captures/attempt). This verifies the excessive capture work: XDamage
previously controlled parking but did not gate active pixel reads. Gating reads
on damage halved motion CPU; reusing XCB and replacing xcap's per-pixel X11
conversion removed another large part. Unsupported X11 pixel formats and
persistent-capture failures retain the original xcap fallback.

### Chrome receiver

Motion snapshots come from the actual headed viewer. Jitter-buffer delay is
`1000 * delta(jitterBufferDelay) / delta(jitterBufferEmittedCount)`; decode and
processing means use the decoded-frame delta. Processing includes decode and
jitter buffering: these columns are not additive. QP is delta(qpSum) divided by
decoded frames; it is not a picture-quality score.

| Run                  | Emitted/decoded delta | Buffer ms/frame | Decode ms/frame | Processing ms/frame | Dropped delta | QP/frame | Received media Mbps |
| -------------------- | --------------------: | --------------: | --------------: | ------------------: | ------------: | -------: | ------------------: |
| Profile baseline     |             3390/3390 |          20.013 |           0.635 |              20.719 |             0 |     26.0 |               0.197 |
| Receiver target zero |             3437/3437 |          19.698 |           0.521 |              20.278 |             0 |     26.0 |               0.199 |
| Damage-driven        |             3430/3430 |          11.887 |           0.565 |              12.524 |             0 |     26.0 |               0.199 |
| Persistent capture   |             3434/3434 |          11.826 |           0.600 |              12.510 |             0 |     26.0 |               0.199 |
| Final, playout delay |             3387/3387 |           0.331 |           0.535 |               0.963 |             0 |     26.0 |               0.200 |

Setting `jitterBufferTarget = 0` alongside the existing `playoutDelayHint = 0`
was insufficient by itself. Chrome's offer advertises the RTP `playout-delay`
extension, but the host did not negotiate or send it. Negotiating it and sending
min/max 0 requests immediate desktop presentation in balanced/responsiveness
modes. Quality mode permits a 100-ms maximum instead. Unsupported receivers
simply do not negotiate the extension. Final measured buffering falls to
0.33 ms/frame, with zero dropped frames and 59.36 unique fps.

The final agent still targets **21,944 kbps** at 1280x720/60 while receiving
approximately 0.200 Mbps on this sparse marker workload. The bitrate controller
is therefore not capped at 0.2 Mbps in this run. No minimum-bitrate padding or
SDP bitrate override was added to inflate traffic toward Sunshine's configured
7.308-Mbps target. This does not establish adequate quality under realistic
video/text motion: that requires a separate quality workload and comparison.

### Per-change benchmark rows

Accepted rows are cumulative, except that the two explicitly discarded
experiments below were each based on persistent capture and were removed
before the final playout-delay change.

| Run                                   | Inputs  | Input p50/p95/p99 ms  | Video p50/p95/p99 ms  | Unique fps | Frame interval p50/p95/p99 ms | Stalls >100ms | CPU idle/motion % | RSS idle/motion MiB | Runtime MiB |
| ------------------------------------- | ------- | --------------------- | --------------------- | ---------- | ----------------------------- | ------------- | ----------------- | ------------------- | ----------- |
| Profile baseline                      | 120/120 | 48.07 / 64.12 / 80.41 | 38.13 / 55.81 / 62.68 | 56.11      | 16.02 / 31.99 / 36.08         | 0             | 0.59 / 116.14     | 53.74 / 59.73       | 15.40       |
| Receiver target zero                  | 120/120 | 44.28 / 56.04 / 64.22 | 37.13 / 54.00 / 55.82 | 56.88      | 16.02 / 31.69 / 35.99         | 0             | 0.78 / 108.96     | 53.82 / 61.22       | 15.40       |
| Damage-driven capture                 | 120/120 | 44.32 / 56.16 / 64.04 | 37.17 / 40.75 / 54.71 | 56.72      | 16.02 / 31.53 / 36.02         | 0             | 0.60 / 59.71      | 43.52 / 55.51       | 15.40       |
| Persistent capture                    | 120/120 | 47.76 / 58.45 / 75.89 | 36.85 / 40.69 / 52.62 | 57.17      | 16.03 / 27.57 / 36.02         | 0             | 0.70 / 45.63      | 43.46 / 55.74       | 15.40       |
| Final, playout delay                  | 120/120 | 36.03 / 52.13 / 53.70 | 23.89 / 37.28 / 39.82 | 59.36      | 16.04 / 20.33 / 25.63         | 0             | 0.58 / 41.97      | 43.79 / 55.47       | 15.40       |
| Sunshine/Moonlight, earlier reference | 120/120 | 28.05 / 40.44 / 58.18 | 16.44 / 24.51 / 38.20 | 58.62      | 16.19 / 24.00 / 33.68         | 0             | 45.35 / 46.27     | 260.29 / 269.81     | 28.94       |

Discarded experiments, not included in the final code:

| Experiment                    | Inputs  | Input p50/p95/p99 ms  | Video p50/p95/p99 ms  | Unique fps | Frame interval p50/p95/p99 ms | Stalls >100ms | CPU idle/motion % | RSS idle/motion MiB | Runtime MiB |
| ----------------------------- | ------- | --------------------- | --------------------- | ---------- | ----------------------------- | ------------- | ----------------- | ------------------- | ----------- |
| Direct RTP elapsed timestamps | 120/120 | 56.03 / 76.44 / 84.37 | 52.26 / 68.83 / 72.10 | 55.73      | 16.05 / 31.99 / 36.05         | 0             | 0.59 / 40.32      | 43.30 / 55.12       | 15.41       |
| OpenH264 single thread        | 120/120 | 44.05 / 56.08 / 56.29 | 36.69 / 39.82 / 45.81 | 57.15      | 16.03 / 28.01 / 35.99         | 0             | 1.09 / 43.03      | 43.36 / 55.03       | 15.40       |

Direct RTP timestamps lowered receiver buffering but worsened pixel latency,
disproving the predicted end-to-end benefit in that run. Single-thread encoding
had only a small backend-time difference (4.93 versus 5.22 ms, including
conversion); this does not justify globally restricting high-resolution
encoding threads. Neither experiment was retained.

Final detector: **14,702 decodable polls**, **3,443 unique video samples**,
**249.17 Hz**, polling p99 **5.27 ms**, capture p99 **0.88 ms**, reader CPU
**1.46%**, clock uncertainty **+/-0.27 ms**. Resource samples: 40 idle over
10.26 s and 236 motion over 59.18 s. Polling quantization remains approximately
4 ms; these single runs do not resolve sub-millisecond improvements.

Final motion CPU beats the earlier Moonlight measurement (41.97% versus 46.27%),
and RSS is lower. **The latency targets remain unmet:** input p50 36.03 ms
versus 28.05 ms, video p50 23.89 ms versus 16.44 ms. The remaining draw/pacing/
presentation delay is not separately attributed by these counters. Color
conversion is still scalar, and no pacer/encoder quality changes were retained.
No impaired-network, physical-display or separate visual-quality validation was
performed for the new playout policy.

Size precision matters: the earlier focus-fixed binary was **16,145,840 bytes**;
the profile baseline was **16,146,608 bytes**; final is **16,150,088 bytes**.
All round to 15.40 MiB, but final is **4,248 bytes (+0.026%) larger** than the
focus-fixed binary. Thus strict zero-byte size growth is not achieved. No new
runtime library or external asset was added.

Artifacts: `~/bench-results/{profile-baseline,receiver-zero,damage-driven,`
`persistent-capture,rtp-clock,single-thread,playout-zero}.{json,md,pixels.log,source.json}`.
Stage logs are the corresponding `*.agent.log` files where present.

## Monotonic attribution and scheduling experiments

**The remaining latency gap is not closed. No scheduling optimization from this
step was retained.** Opt-in attribution tooling is retained; the final pacer is
unchanged from `f60b68a`. All runs below matched 120/120 inputs. Motion runs remain
720p software H.264 with the existing settings, sequentially on the same displays.

### Baseline attribution (`attribution-full`)

3324 matched source/native/rVFC/client-pixel frame observations; 120 input chains.
Source/browser clocks were independently aligned to CLOCK_MONOTONIC. Source
uncertainty was ±0.23 ms, viewer ±0.29 ms. Native stages use clock_gettime directly
(damage receipt is reconstructed from the Instant taken in the XDamage drain).
Encode includes preprocessing/color conversion. RTP write start/end surrounds
write_sample_with_extensions, not a packet sniffer observation. Segment medians
are not additive, and decoder processingDuration is a duration, not a separately
observed decode-end timestamp.

| Segment                                  | p50 ms | p95 ms |
| ---------------------------------------- | -----: | -----: |
| Source draw → damage received            |  1.737 |  2.447 |
| Damage received → capture start          |  0.036 |  0.253 |
| Capture start → end                      |  1.682 |  2.605 |
| Capture end → encode start               |  8.744 | 15.533 |
| Encode start → end                       |  4.345 |  6.489 |
| Encode end → RTP write start             |  0.137 |  0.391 |
| RTP write start → end                    |  0.042 |  0.064 |
| RTP write end → rVFC receiveTime         |  0.230 |  0.644 |
| receiveTime → presentationTime           |  0.700 |  3.600 |
| Decode processingDuration                |  0.400 |  0.700 |
| presentationTime → expectedDisplayTime   | 19.200 | 31.100 |
| expectedDisplayTime → first client pixel | -9.917 |  9.360 |
| presentationTime → first client pixel    |  5.267 | 13.275 |
| Source draw → first client pixel         | 25.033 | 32.803 |
| Client XTest → viewer DOM keydown        |  0.729 |  3.811 |
| Viewer DOM → datachannel send            |  0.100 |  0.200 |
| Datachannel send → agent receive         |  0.205 |  0.672 |
| Agent receive → inject start             |  0.052 |  0.336 |
| Inject start → end                       |  0.165 |  0.693 |
| Inject end → source DOM                  |  0.270 |  0.773 |
| Source DOM → repaint                     |  7.900 | 16.100 |
| XTest → source repaint                   |  9.469 | 17.958 |

**Browser prediction caveat:** expectedDisplayTime was later than already-visible
pixels in 2852/3324 observations; presentation→expected also had 93 negative
deltas. These raw metadata predictions are not physical compositing latency and
do not establish that video compositing adds a full refresh. Three send→receive
and two injection→source DOM deltas were negative near the clock uncertainty;
none were clamped or silently discarded. See the raw attribution files for
individual segment counts and negative counts. No WebCodecs change was made.

### Verified reachability and attempted changes

The active capture loop's XDamage receipt→capture start is only 0.036 ms p50.
The encode loop in media.rs calls DamagePacer::on_frame and waits on
DamagePacer::next_deadline when throttled; adaptive.rs uses the previous encode
start plus the frame interval. As a disproof check against blaming that timer,
the 3362 native frames with >1 ms queueing were compared with a deadline
predicted from the preceding encode start plus 1000/60 ms: residual p50/p95
was 0.066/0.238 ms (observed queue median 8.862 ms). This supports the timer as
a contributor, but the experiments below did not prove a user-visible win.
The first motion marker after idle waited only 0.457 ms from capture end to
encode start in this baseline; this does not test every keepalive coincidence.

Keys already use an ordered channel; pointer moves already use ordered=false,
maxRetransmits=0 in remote-session.ts. The measured receive→worker wait and
injection duration do not explain an 8 ms gap. A separate 1000-sample direct
XTest Space-down + XFlush submission microbenchmark on the disposable host
display measured 0.005533/0.013622 ms p50/p95 versus Enigo dispatch's
0.165/0.693 ms. The direct test excludes server execution/DOM repaint, so it is
not an end-to-end comparison. No input backend/channel change was made.

| Run / disposition                   | Input p50/p95/p99 ms  | Video p50/p95/p99 ms  |   FPS | CPU idle/motion % | RSS idle/motion MiB | Stalls >100ms |
| ----------------------------------- | --------------------- | --------------------- | ----: | ----------------- | ------------------- | ------------: |
| Instrumented baseline               | 35.89 / 44.29 / 48.07 | 24.98 / 33.16 / 46.00 | 59.51 | 0.68 / 40.46      | 43.38 / 54.90       |             0 |
| Phase-stable deadlines; discarded   | 44.05 / 76.19 / 88.22 | 56.20 / 64.51 / 76.24 | 59.67 | 0.50 / 41.86      | 43.23 / 54.49       |             1 |
| Bounded one-frame credit; discarded | 40.08 / 60.25 / 64.71 | 38.94 / 44.26 / 49.45 | 59.76 | 0.60 / 42.56      | 43.67 / 54.93       |             0 |
| Final, original pacer, tracing off  | 32.24 / 48.05 / 55.12 | 22.93 / 31.11 / 37.39 | 59.52 | 0.60 / 39.21      | 43.42 / 55.03       |             0 |
| Earlier Moonlight reference         | 28.05 / 40.44 / 58.18 | 16.44 / 24.51 / 38.20 | 58.62 | 45.35 / 46.27     | 260.29 / 269.81     |             0 |

The first candidate preserved timer phase across small overshoots but still had
7.922/9.642 ms capture→encode wait. Source draw→damage shifted to
33.721/34.955 ms in that run. The second candidate permitted one bounded burst
frame while preserving the sustained FPS limit; capture→encode improved to
4.163/11.221 ms, but presentation→pixels rose to 12.515/21.951 ms and headline
latency worsened. These upstream/downstream shifts are observed, not proven to
be caused by either pacer. Both candidates and their candidate-specific tests
were removed, rather than retaining an unverified change.

The final row is a new observation with the original pacer, not an attributable
optimization. It has 14733 polls at 249.70 Hz, polling p99 4.88 ms, read p99
0.69 ms, reader CPU 1.43%, and source clock uncertainty ±0.25 ms. Runtime is
16,161,320 bytes (15.41 MiB), +11,232 bytes versus the previous final binary,
from diagnostic support; strict zero-byte size growth is not achieved. No new
runtime dependency was added. The approximately 4 ms detector quantization and
unisolated instrumentation overhead remain significant. No separate visual or
picture-quality check was performed.

Next recommended investigation: repeat paired baseline/candidate runs while
separately tracking source draw→damage and presentation→pixels variance, before
attributing a small headline difference or attempting a renderer replacement.
Initial `attribution-baseline` output was input-only because fixed-row marker
sampling missed the actual marker; the retained sampler discovers/caches its
row. That incomplete run is not used for the frame table.

Artifacts: `~/bench-results/{attribution-full,phase-stable,burst-credit,`
`final-attribution-step}.{json,md,pixels.log,source.json,agent.log}`;
the first three also have `.attribution.{json,md}`. Tooling commit: `c9e1341`.

## Five-run pacer comparison

**Initial decision under the strict CPU gate: retain the current pacer.** This
decision was superseded by the user-approved tradeoff below. This comparison
uses five fresh trials per group, not the preceding
single-run experiments. All **20/20 runs were valid**, matching **600/600 inputs
per group, 2400/2400 total**. No outlier was excluded and no run was retried.

### Protocol

- Sequential blocks in this order: current ×5, token bucket ×5, half interval
  ×5, Sunshine/Moonlight ×5. No competing stream or build ran during measurement.
- 1280×720, software H.264, existing defaults, 120 randomized input trials per
  run, fixed seed 1729, ten-second static phase and approximately one-minute
  motion phase. Chromium 151.0.7922.34 was used throughout.
- Source/viewer browsers are recreated between trials; each host persists within
  its five-run group. Nanoctl was restarted between candidate groups. The
  Moonlight stream was restarted for the reference; Sunshine remained listening
  while inactive during nanoctl trials. The native Moonlight client persists
  across its five trials. All attribution tracing was disabled.
- Candidate binaries were built before measurements using the same release
  media build wrapper and unchanged viewer assets. No runtime policy flag or
  candidate implementation remains in the product.
- Gate declared before results: lower median input **and** video p50 than current,
  with no higher median input/video p95 or motion CPU. These are point-estimate
  comparisons; no significance test or noninferiority margin is implied.

Policies:

1. **Current:** previous encode start + full frame interval (16.667 ms at 60 fps).
2. **Token bucket:** refill at configured FPS, capacity two frames; encode fresh
   damage immediately when a token is available, otherwise wait until refill.
   Idle/refresh and forced-keyframe behavior is otherwise preserved. Unit tests
   cover jitter, bounded bursts, sustained rate, idle refill and zero-FPS clamping.
3. **Half interval:** replace the minimum wait with half the configured frame
   interval (8.333 ms at 60 fps). This is not an average-rate token bucket: on a
   higher-rate damage source it can permit twice the configured rate. It is only
   an experimental candidate here, not a retained change.

### Results

Every cell is **median of per-run metrics [minimum–maximum across five runs]**,
not a pooled percentile or confidence interval. CPU is host motion-phase mean,
with one core = 100%.

| Policy / product                    | Input p50 ms        | Input p95 ms        | Video p50 ms        | Video p95 ms        | Motion CPU %        |
| ----------------------------------- | ------------------- | ------------------- | ------------------- | ------------------- | ------------------- |
| Current                             | 36.01 [32.36–44.03] | 48.35 [44.36–76.46] | 23.96 [22.46–50.75] | 36.07 [31.11–63.15] | 38.59 [36.18–44.86] |
| Token bucket                        | 32.14 [28.37–44.70] | 44.09 [36.37–56.16] | 21.13 [19.95–35.45] | 38.11 [22.04–38.55] | 37.03 [34.35–39.81] |
| Half interval                       | 32.05 [28.38–40.10] | 40.27 [40.09–56.07] | 20.35 [20.09–35.80] | 23.90 [22.63–39.43] | 41.13 [39.34–42.18] |
| Sunshine/Moonlight, fresh reference | 36.05 [28.07–48.05] | 44.06 [36.30–64.41] | 23.72 [18.08–46.53] | 37.05 [23.51–50.94] | 48.43 [44.82–52.62] |

| Policy / product   | Unique FPS          | Motion RSS MiB         | Runtime bytes |
| ------------------ | ------------------- | ---------------------- | ------------: |
| Current            | 59.39 [59.21–59.48] | 92.95 [55.41–102.67]   |    16,161,320 |
| Token bucket       | 59.98 [59.88–60.00] | 102.25 [54.67–138.14]  |    16,161,832 |
| Half interval      | 59.93 [59.76–60.00] | 103.27 [54.86–137.73]  |    16,161,320 |
| Sunshine/Moonlight | 59.67 [56.88–59.93] | 283.53 [282.30–283.53] |    30,349,491 |

- **Token bucket rejected:** median video p95 is **+2.03 ms** versus current,
  despite lower input/video p50 and CPU. Its input p95 also improved.
- **Half interval rejected:** latency medians improve, but median motion CPU is
  **+2.53 percentage points (+6.57%)** versus current. It still uses less CPU than
  Moonlight here, but that is not the declared retention condition.
- Ranges overlap substantially. These gates do not establish that either
  regression is statistically significant; they prevent retaining a candidate
  without satisfying the requested no-regression rule on the reported medians.
- Fresh Moonlight medians are 36.05/23.72 ms input/video, not the earlier single
  run's 28.05/16.44 ms. Neither that old point estimate nor the approximately
  8 ms gap should be treated as stable across these runs. Sequential blocks
  also leave order/time drift as a limitation; this is not a paired crossover.
- RSS grew across reconnects for all nanoctl policies. Current motion RSS by run
  was 55.41, 79.90, 92.95, 101.42, 102.67 MiB; token and half reached approximately
  138 MiB. Allocator high-water behavior versus retained allocations was not
  investigated, so these are not claimed as proven memory leaks or policy causes.

All runs had zero >100 ms stalls. Detector frequency ranged 248.52–249.90 Hz,
poll interval p99 4.54–6.73 ms, and source clock uncertainty ±0.22–0.40 ms.
Approximately 4 ms polling quantization remains significant. Nanoctl received
approximately 0.20 Mbps on this sparse marker workload. No picture-quality or
separate visual review was performed.

### Artifacts and checks

`run.ts --repeat N` preserves each trial as `PRODUCT-run-I.{json,md,pixels.log,source.json}`
and writes `PRODUCT.repeat.{json,md}`. Existing run JSON is refused, and missing,
invalid or non-finite required metrics make the aggregate incomplete. Per-run
results and failure descriptions are never replaced with synthetic successes.

Artifacts: `~/bench-results/stats-{current,token,half,moonlight}-run-{1..5}.*`
and `~/bench-results/stats-{current,token,half,moonlight}.repeat.{json,md}`.
Experiment binaries, source patches, agent logs and run logs remain under
`~/bench-candidates/`. Binary SHA-256 identities:

```text
current 14bb47df41db744438b914b2edf5d5cf99c6895e798d414668abc00ff37abd01
token   8fe3c5dba39d4df8174377722fed91b723aa2be65bc71d1b6c38d99b4e12cd13
half    c17ac757458751b488b9b43805e2b42a88bfa3f001c02e8da3533d6041f15754
```

Repeat harness commit: `45b4bd0`. Full Bun tests: 85 passed, zero failed;
focused changed-file lint and TypeScript checking passed. Candidate adaptive
tests: 30 passed for token bucket, 29 for half interval. After rejection,
adaptive.rs was restored byte-for-byte; the prior passing unchanged-code Rust
clippy/RTC/media matrix (68 RTC tests, 115 media tests, one ignored) was reused
rather than rerun without changed inputs. The release artifact was rebuilt to
replace the experimental binary. No native product-code diff remains.

## User-approved half-frame pacing and reconnect RSS

The user explicitly accepted the half-interval candidate's CPU tradeoff.
**Half-frame minimum pacing is now retained**, commit `67fd0da`, with pending
damage, exact deadline, forced-keyframe, empty-input and zero-FPS coverage.
This is the same policy/binary used in the five-run half-interval comparison:
input p50/p95 32.05/40.27 ms, video p50/p95 20.35/23.90 ms, motion CPU 41.13%.
The approximately +2.53 CPU percentage points versus the old pacer are accepted;
CPU remains below the same-session Moonlight median of 48.43%. These latency
figures are the prior five-run measurements, not a new post-trim latency study.

### Reconnect experiment

Before and final-after runs each use a fresh agent, the retained half-frame
policy, the same moving 720p/60 Hz marker source and real headed Chromium viewer.
Each of 20 cycles creates a fresh viewer context, verifies video, runs ten
seconds of motion, closes the context/WebSocket, waits three seconds, then reads
agent-only VmRSS. Browser processes persist within a run. Initial agent RSS was
10.27 MiB for both runs. All 20 cycles completed in each run; minimum video frame
counts were 547 before and 600 after.

**RSS after disconnect and settling, MiB:**

| Cycle | Before trimming | Final build with trimming |
| ----: | --------------: | ------------------------: |
|     1 |           40.36 |                     18.02 |
|     2 |           67.59 |                     18.89 |
|     3 |           91.12 |                     29.55 |
|     4 |           97.18 |                     19.59 |
|     5 |          106.63 |                     21.66 |
|     6 |          115.74 |                     22.64 |
|     7 |          118.53 |                     23.59 |
|     8 |          121.38 |                     25.69 |
|     9 |          123.40 |                     28.64 |
|    10 |          123.40 |                     28.67 |
|    11 |          123.40 |                     28.66 |
|    12 |          124.59 |                     28.91 |
|    13 |          124.63 |                     29.16 |
|    14 |          124.66 |                     29.16 |
|    15 |          125.89 |                     30.16 |
|    16 |          126.76 |                     31.04 |
|    17 |          123.25 |                     31.06 |
|    18 |          124.11 |                     31.07 |
|    19 |          147.26 |                     41.67 |
|    20 |          161.20 |                     32.11 |

Connected RSS at the end of cycle 20 also fell, 161.20 → 70.14 MiB. Open FDs
remained 12 throughout both runs. Post-disconnect threads were 12–13, consistent
with a small runtime worker pool rather than one surviving thread per session.

### Attribution and fix

The large increase is **predominantly freed glibc arena residency**, not evidence
of approximately 100 MiB of live encoders/frames per reconnect. Evidence:

- The reached WebSocket cleanup path awaits `VideoPipeline::stop()`, including
  its blocking capture/encoder worker, then closes and drops `HostPeer` before
  allocator cleanup. The existing worker-join regression remains covered.
- The real WebRTC controller/host regression now checks weak references after
  close/drop: both peer connection and video track are actually released.
- A diagnostic build sampled `mallinfo2` around each trim for 40 cycles. At
  cycle 40, glibc reported **182.21 MiB free arena bytes** versus **1.86 MiB
  in-use plus mmap bytes**. In-use accounting includes thread caches; it is
  not a precise live-object census. `malloc_trim` cannot free live allocations.
- That diagnostic process's post-disconnect RSS ended at **32.95 MiB** after
  40 cycles. Cycles 36–40 were **32.88, 32.90, 32.94, 32.95, 32.95 MiB**.
  This is a practical plateau over the observed window, not proof that every
  smaller or longer-running leak is absent. The final clean run also retains
  transient RSS variation, including cycle 19's 41.67 MiB.

The retained fix calls **glibc `malloc_trim(0)` once after peer/media teardown**,
on a blocking worker rather than the async executor or frame loop. It returns
free pages to the OS without changing live buffers or imposing a heap policy
on active encoding. The 40 diagnostic calls took **2.02 ms median, 4.13 ms max**.
Other platforms/allocators are unchanged. Tests cover repeated cleanup with no
buffers, live buffers and freed buffers, in addition to actual peer/track release.

Temporary `mallinfo2` probes were removed before the final 20-cycle run. The
final binary imports `malloc_trim@GLIBC_2.2.5`, not `mallinfo2@GLIBC_2.33`, avoiding
a new glibc 2.33 minimum just for diagnostics. Final runtime size is 16,172,120
bytes, +10,800 bytes (+0.067%) for cleanup support; pacing alone did not grow it.

### Artifacts and verification

- Baseline: `~/bench-results/rss-before.json`, logs
  `~/bench-candidates/rss-before{,.agent}.log`.
- Final production build: `~/bench-results/rss-final-clean.json`, logs
  `~/bench-candidates/rss-final-clean{,.agent}.log`.
- Diagnostic cycles 1–20 and 21–40: `~/bench-results/rss-after.json` and
  `rss-after-continuation.json`; allocator counters in
  `~/bench-candidates/rss-after.agent.log`, diagnostic source diff in
  `rss-instrumentation.patch`. One interim run (`rss-final.json`) was stopped
  after one cycle when a test-import error prevented the final rebuild; it is
  preserved but excluded from these comparisons.
- Final checks: clippy all-targets for both RTC and media, 69 RTC tests,
  117 media tests (one ignored), 87 Bun tests, focused reconnect lint/typecheck,
  release build and clean diff-whitespace check. An initial media-clippy test
  import error was fixed before the final passing checks and production run.
- `scripts/bench/reconnect.ts` and README document reproducing the cycle probe.
  No new picture-quality or separate visual review was performed.

## Real-content quality and received bitrate

Completed 27 baseline runs (three products × text/drag/gradient × three repeats),
then nine nanoctl runs with **only software H.264 color metadata corrected**.
All 36 were valid, with 1,428 matched quality pairs (38–40 per run).
Sequential blocks: Moonlight, nanoctl baseline, screen-relay, nanoctl correction.
Moonlight was closed before measuring the other products. No builds, tests,
competing streams or full-frame quality readers ran during timed motion phases.
Defaults otherwise match the preceding setup: 720p/60, software H.264; Moonlight
logged a 10,000-kbps request, nanoctl's configured ceiling is 24 Mbps, screen-relay
retains its default 1/8/40-Mbps policy. Targets are not received byte rates.

Cells below are medians across three runs: per-run median frame SSIM/PSNR and
video latency, unique FPS, measured received bitrate and average motion CPU.
CPU is one core = 100%. Min–max spreads and raw per-run data are preserved in
`~/bench-results/content/content-*.repeat.{json,md}`. These are not confidence
intervals; blocks were sequential, not randomized.

| Workload | Product           | SSIM Y  | PSNR Y dB | Received Mbps | FPS   | Video p50 ms | Motion CPU % |
| -------- | ----------------- | ------- | --------- | ------------- | ----- | ------------ | ------------ |
| Text     | nanoctl before    | 0.99513 | 39.09     | 0.89          | 59.62 | 35.68        | 52.87        |
| Text     | screen-relay      | 0.99958 | 52.22     | 3.36          | 59.70 | 52.78        | 34.17        |
| Text     | Moonlight         | 0.99934 | 48.42     | 5.30          | 59.84 | 40.33        | 62.80        |
| Text     | nanoctl corrected | 0.99522 | 39.67     | 0.89          | 59.53 | 19.80        | 51.23        |
| Drag     | nanoctl before    | 0.99382 | 39.19     | 0.74          | 59.84 | 19.52        | 36.40        |
| Drag     | screen-relay      | 0.99957 | 52.39     | 7.53          | 59.70 | 57.49        | 35.78        |
| Drag     | Moonlight         | 0.99916 | 48.57     | 7.70          | 59.75 | 29.38        | 60.16        |
| Drag     | nanoctl corrected | 0.99510 | 40.99     | 0.73          | 59.70 | 36.38        | 40.55        |
| Gradient | nanoctl before    | 0.98548 | 27.67     | 3.26          | 59.53 | 36.40        | 50.92        |
| Gradient | screen-relay      | 0.99404 | 47.66     | 16.57         | 59.70 | 62.13        | 48.23        |
| Gradient | Moonlight         | 0.99293 | 46.49     | 7.39          | 58.10 | 22.06        | 70.02        |
| Gradient | nanoctl corrected | 0.99107 | 42.65     | 3.26          | 59.11 | 36.32        | 49.16        |

### What the quality fix corrects

This is a reached software-encoder color-description bug, not evidence of a
0.2-Mbps real-content REMB cap. The actual configured software backend calls
`I420Frame::read_rgba`, whose integer coefficients produce limited-range BT.601
YUV. `create_encoder` previously omitted VUI. Actual Chrome VideoFrame metadata
in `content-nanoctl-text-run-1.json` reported limited-range **BT.709** matrix and
transfer. The controller/quantizer were deliberately left unchanged to challenge
the alternative explanation that the whole deficit was insufficient bitrate.

The sole change signals BT.601/SMPTE-170M matrix, limited range, BT.709/sRGB
primaries and sRGB transfer. Actual corrected VideoFrame metadata reports
`matrix=smpte170m`, `fullRange=false`, `primaries=bt709`,
`transfer=iec61966-2-1`. The initialized OpenH264 parameters are regression-tested
for all three latency modes. No capture, rate-control, QP, pacing or thread
setting changed. Software encoding on Linux was measured; hardware paths are
untouched and were not evaluated here.

The gradient gain is **27.67 → 42.65 dB (+14.98 dB)** at unchanged 3.26 Mbps.
Drag improves 1.80 dB and text 0.58 dB. Text SSIM ranges overlap; its tiny SSIM
gain alone is not persuasive. The correction is retained for correct color
description and repeatable PSNR/gradient improvement, not claimed as latency or
CPU optimization. Drag's after-block latency and CPU are **worse**; text's latency
is better. Refresh-sized timing shifts remain and no causal performance claim
is made from these blocks. Both release binaries are exactly 16,172,120 bytes;
copies are `~/bench-candidates/nanoctl-quality-{before,vui}`.

Nanoctl still loses quality to both competitors and text CPU to screen-relay.
No QP hard cap, forced padding, rate-control change or optional SIMD/CPU
optimization was retained or attempted after the color correction. Further
quality/CPU tuning remains unfinished; the table must not be read as parity.

### Measurement scope and evidence

- The source workloads are deterministic canvas simulations, not native editor
  automation or prerecorded video. Every frame retains checksummed IDs.
- Timing/CPU use a 20-second motion phase. A separate ten-second paired phase
  captures both displays through persistent XShm, verifies IDs after full reads,
  and scores a native 1120x360 ROI excluding markers/browser chrome. This avoids
  charging frame capture, PPM writes or FFmpeg analysis to host motion CPU.
- Scores are FFmpeg gray-luma 8x8 SSIM and peak-255 PSNR; no chroma score or
  perceptual-model claim. An identical-source self-check printed
  `SSIM Y:1.000000 ... All:1.000000` and `PSNR y:inf average:inf`.
- Browser viewports are normalized to 1280x720 at scale 1, including compensating
  Chromium chrome on WM-less Xvfb. Scaled viewers are rejected. Preliminary
  `probe-*` outputs at scale 0.688/0.879 were geometry-contaminated and are
  excluded. A preliminary Moonlight tcpdump shutdown probe also ran too long;
  only the named `content-*` runs are used above.
- Container Xvfb's different UID required dummy-data XShm segments accessible
  across UIDs. README documents permissions and the host-IPC trust boundary.
- Nanoctl bitrate is video inbound-RTP payload counters. Screen-relay uses CDP
  received WebSocket payload bytes including protocol/control messages, excluding
  WS/TCP headers. Moonlight uses video-port 49009 incoming loopback UDP payload
  lengths, including Sunshine/FEC headers but excluding UDP/IP. Its raw PCAPs and
  zero-kernel-drop counters are retained. These scopes are not wire-equivalent.
- Per-run JSON, source timelines, pixel logs, matched PPMs, quality logs and PNGs
  are under `~/bench-results/content/`. Prefixes are
  `content-{nanoctl,screen-relay,moonlight}-{text,drag,gradient}-run-{1,2,3}`;
  corrected prefixes are `content-nanoctl-vui-{text,drag,gradient}-run-{1,2,3}`.
  Text crops append `.quality-text-source.png` / `.quality-text-client.png`.
  Crops have been inspected; Japanese glyph fallback boxes occur in the source
  too and are not evidence of an encoder defect.
- Checks: 98 Bun tests; focused oxlint/types; strict C compilation and unavailable
  display failure; clippy all-targets RTC/media; 69 RTC tests; 118 media tests,
  one preexisting X11 test ignored; release build. No new impaired-link,
  hardware-encoder or reconnect-RSS campaign was performed.

## Software quantizer bounds: real-content quality follow-up

Fresh 1280x720 software-H.264 comparisons, three sequential runs per workload
and variant, with no competing streams/builds during measurement. Values are
medians of per-run frame medians (quality/video), or per-run motion means
(CPU/bitrate); not pooled percentiles or confidence intervals. Full min–max
spreads remain in `~/bench-results/content/quality-gap-*.repeat.{json,md}`.
All 27 runs completed and were valid. Quality captures/scoring remain separate
from the 20-second motion resource/latency phase. No outlier was discarded.

| Workload | Variant            | SSIM Y  | PSNR Y dB | Received Mbps | FPS   | Video p50 ms | Motion CPU % |
| -------- | ------------------ | ------- | --------- | ------------- | ----- | ------------ | ------------ |
| Text     | Before             | 0.99513 | 39.67     | 0.90          | 58.85 | 20.60        | 59.72        |
| Drag     | Before             | 0.99497 | 40.99     | 0.73          | 59.69 | 35.77        | 35.61        |
| Gradient | Before             | 0.99107 | 42.71     | 3.26          | 59.67 | 36.46        | 46.65        |
| Text     | QP 16–51, rejected | 0.99768 | 42.44     | 1.51          | 59.19 | 19.96        | 56.25        |
| Drag     | QP 16–51, rejected | 0.99777 | 43.44     | 3.27          | 59.69 | 36.49        | 44.47        |
| Gradient | QP 16–51, rejected | 0.99441 | 46.03     | 7.17          | 59.00 | 20.97        | 52.17        |
| Text     | QP 20–51, kept     | 0.99715 | 41.73     | 1.10          | 59.26 | 20.08        | 51.69        |
| Drag     | QP 20–51, kept     | 0.99721 | 42.86     | 1.89          | 59.79 | 20.33        | 37.06        |
| Gradient | QP 20–51, kept     | 0.99346 | 44.82     | 5.17          | 59.68 | 36.31        | 48.57        |

### Reached cause, not a new bitrate floor

The software factory uses `EncoderConfig::new()`, whose OpenH264 wrapper default
is QP 0–51. For bitrate-controlled screen content, the source vendored by
`openh264-sys2 0.9.7` treats a nonpositive minimum in `ParamValidation()` as a sentinel and
replaces **both** bounds with `MIN_SCREEN_QP=26`, `MAX_SCREEN_QP=35`.
The reached path is `SoftwareEncoder::new` → `create_encoder` → source-built
OpenH264 initialization with `SCREEN_CONTENT_REAL_TIME` and `RC_BITRATE_MODE`.
In a diagnostic text run, 1,911 native attempts reported last-frame macroblock
QP min/median/max **26/26/30**, while the actual encoder target rose from
**4,000 to 24,000 kbps** (median 24,000). Raising the target did not lower QP
below 26. The low payload rate therefore was not evidence of a 0.2–0.9-Mbps
controller ceiling. Chrome's incoming-bandwidth field is diagnostic, not a
substitute for the reached encoder target or received RTP bytes.

| Text diagnostic                   | Native QP min/median/max | Encoder target min/median/max kbps | Chrome availableIncomingBitrate min/median/max Mbps |
| --------------------------------- | ------------------------ | ---------------------------------- | --------------------------------------------------- |
| Before, 1,911 attempts            | 26 / 26 / 30             | 4000 / 24000 / 24000               | 0.576 / 0.676 / 0.939                               |
| QP 20–51, 1,932 nonempty attempts | 20 / 20 / 30             | 4000 / 21926 / 21926               | 0.676 / 1.170 / 1.633                               |

Chrome exposed incoming estimates in 19 samples per probe, not as a guaranteed
link-capacity measurement. It reported more than one nominated pair; the added
transport snapshots identify selection explicitly. These diagnostic probes are
not mixed into the three-run resource medians. Their artifacts use prefixes
`quality-gap-profile` and `quality-gap-qp20-profile`.

Explicit QP 16–51 improved fidelity, but drag CPU rose **8.86 points**, failing
the approximately five-point budget. Explicit QP 20–51 is retained: PSNR improves
**2.06/1.86/2.11 dB** for text/drag/gradient, with motion CPU changes
**−8.03/+1.45/+1.91 points**, and no median video-p50 increase. Text CPU has a
large baseline spread (45.54–64.94%), so its reduction is not attributed to this
patch. Likewise, refresh-sized latency swings are not claimed as optimizations.
The retained setting **does not close the remaining competitor quality gap**.

Maximum QP remains 51 so allocation cuts can sacrifice fidelity/frame output;
the adaptive controller, starting bitrate, bitrate bounds, feedback negotiation,
frame pacing, VUI and encoder complexity are unchanged. Regression tests read
back effective QP bounds and bitrate mode for every latency mode and exercise
the actual software encoder after a 4,000→250-kbps allocation cut. OpenH264 can
back off by skipping frames, not only by raising QP. No new impaired-network
campaign is claimed. No idle-only rate-control bypass was introduced.

Native QP/target diagnostics are opt-in (`RUST_LOG=info,nanoctl::encoder=debug`)
and disabled for the comparisons. Browser probes now retain nominated candidate
pairs and transports, including optional `availableIncomingBitrate`; received
media Mbps remains strictly inbound-video RTP deltas. Tests prevent candidate
pair or audio counters being counted as video and reject invalid/reset counters.

### Separately evaluated: nearest-code I420 rounding, rejected

After retaining QP 20–51, an encoder-free round-trip of a captured text ROI
implicated the truncating fixed-point RGB→I420 transform as another fidelity
limit: FFmpeg round-trip PSNR was 38.89 dB with existing truncation, 42.36 dB
with nearest-code rounding. This is a diagnostic proxy, not a Chrome quality
score. A four-addition rounding variant was then measured in the real viewer,
without changing quantizer bounds, pacing, rate control or VUI.

Its first sequential nine-run block was:

| Workload | SSIM Y  | PSNR Y dB | Received Mbps | FPS   | Video p50 ms | Motion CPU % |
| -------- | ------- | --------- | ------------- | ----- | ------------ | ------------ |
| Text     | 0.99893 | 47.00     | 1.11          | 59.05 | 36.75        | 54.97        |
| Drag     | 0.99879 | 47.69     | 1.89          | 59.36 | 20.05        | 43.79        |
| Gradient | 0.99396 | 47.03     | 5.18          | 59.46 | 36.39        | 49.34        |

This initially missed both text p50 and drag CPU budgets relative to the older
QP-20 block. Before attributing the 16.67-ms text shift to rounding, a fresh,
unchanged QP-20 control was run three times: **36.58 ms [19.58–36.68]**, CPU
**51.14% [46.22–52.62]**, PSNR **41.72 dB**. This disproves treating a 20→36-ms
block shift alone as proof of a rounding-induced delay. It does not identify
the cause of the presentation-phase variation.

A further comparison was declared **before running it**: three pairs per
workload, fresh agents each run, order control/rounding, rounding/control,
control/rounding. No builds, competing streams or code changes during the
18-run comparison. All 18 runs were valid. Medians of per-run metrics:

| Workload | Variant           | SSIM Y  | PSNR Y dB | Received Mbps | FPS   | Video p50 ms | Motion CPU % |
| -------- | ----------------- | ------- | --------- | ------------- | ----- | ------------ | ------------ |
| Text     | QP-20 control     | 0.99715 | 41.73     | 1.10          | 59.37 | 36.38        | 52.05        |
| Text     | Rounded, rejected | 0.99883 | 47.00     | 1.11          | 59.26 | 19.94        | 52.52        |
| Drag     | QP-20 control     | 0.99723 | 42.85     | 1.89          | 59.84 | 20.46        | 36.06        |
| Drag     | Rounded, rejected | 0.99871 | 47.71     | 1.90          | 59.42 | 20.04        | 49.37        |
| Gradient | QP-20 control     | 0.99345 | 44.81     | 5.17          | 58.71 | 36.44        | 53.92        |
| Gradient | Rounded, rejected | 0.99395 | 47.04     | 5.18          | 58.80 | 36.30        | 53.37        |

The quality gains reproduce, and paired median video p50 does not regress.
However, drag CPU rises **13.31 points**, versus the approximately five-point
budget (first block: +6.74 points). Therefore **rounding is rejected**, not
quietly retained for its attractive quality numbers. These measurements do
not isolate the cause of the extra CPU between generated conversion code,
content-dependent encoder work and other per-run effects. No such cause is
claimed confirmed. No outlier was removed; the initial block, unchanged control
and paired block are all preserved under `quality-gap-round-*`,
`quality-gap-qp20-control-text*` and `quality-gap-paired-*`.

There were **57 valid comparison/control runs**, plus two separate diagnostic
probes. Native-pixel source/client text crops were inspected; examples include
`quality-gap-qp20-text-run-2.quality-text-client.png` and the rejected variant's
`quality-gap-round-text-run-2.quality-text-{source,client}.png` in the content
artifact directory. The rejected rounding patch and executable are preserved
under `~/bench-candidates/quality-gap-round{.patch,}` for investigation.

Final product source and executable are restored exactly to the retained
QP-20 variant. Idle-only refinement, SDP bitrate hints and transport-CC changes
were **not implemented**; the remaining quality gap is explicitly unfinished.
No new input-latency study, impaired-link or hardware-encoder run is claimed.
The original QP-20 comparison meets its median gates, but unchanged later
controls still show refresh-sized timing variation: this is not a universal
latency guarantee or an attributable latency improvement.
