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

| Stage, mean ms/operation | Profile baseline | Persistent capture | Final, playout delay |
| --- | ---: | ---: | ---: |
| X11 capture | 5.120 | 2.168 | 2.003 |
| Frame comparison, per capture | 0.131 | 0.115 | 0.102 |
| Resize/preprocess, per encode attempt | 0.0003 | 0.0007 | 0.0003 |
| RGBA to I420 | 1.598 | 1.522 | 1.394 |
| OpenH264/backend excluding conversion | 3.996 | 3.702 | 3.445 |
| Packetize/send sample | 0.061 | 0.058 | 0.055 |

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

| Run | Emitted/decoded delta | Buffer ms/frame | Decode ms/frame | Processing ms/frame | Dropped delta | QP/frame | Received media Mbps |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Profile baseline | 3390/3390 | 20.013 | 0.635 | 20.719 | 0 | 26.0 | 0.197 |
| Receiver target zero | 3437/3437 | 19.698 | 0.521 | 20.278 | 0 | 26.0 | 0.199 |
| Damage-driven | 3430/3430 | 11.887 | 0.565 | 12.524 | 0 | 26.0 | 0.199 |
| Persistent capture | 3434/3434 | 11.826 | 0.600 | 12.510 | 0 | 26.0 | 0.199 |
| Final, playout delay | 3387/3387 | 0.331 | 0.535 | 0.963 | 0 | 26.0 | 0.200 |

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

| Run | Inputs | Input p50/p95/p99 ms | Video p50/p95/p99 ms | Unique fps | Frame interval p50/p95/p99 ms | Stalls >100ms | CPU idle/motion % | RSS idle/motion MiB | Runtime MiB |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Profile baseline | 120/120 | 48.07 / 64.12 / 80.41 | 38.13 / 55.81 / 62.68 | 56.11 | 16.02 / 31.99 / 36.08 | 0 | 0.59 / 116.14 | 53.74 / 59.73 | 15.40 |
| Receiver target zero | 120/120 | 44.28 / 56.04 / 64.22 | 37.13 / 54.00 / 55.82 | 56.88 | 16.02 / 31.69 / 35.99 | 0 | 0.78 / 108.96 | 53.82 / 61.22 | 15.40 |
| Damage-driven capture | 120/120 | 44.32 / 56.16 / 64.04 | 37.17 / 40.75 / 54.71 | 56.72 | 16.02 / 31.53 / 36.02 | 0 | 0.60 / 59.71 | 43.52 / 55.51 | 15.40 |
| Persistent capture | 120/120 | 47.76 / 58.45 / 75.89 | 36.85 / 40.69 / 52.62 | 57.17 | 16.03 / 27.57 / 36.02 | 0 | 0.70 / 45.63 | 43.46 / 55.74 | 15.40 |
| Final, playout delay | 120/120 | 36.03 / 52.13 / 53.70 | 23.89 / 37.28 / 39.82 | 59.36 | 16.04 / 20.33 / 25.63 | 0 | 0.58 / 41.97 | 43.79 / 55.47 | 15.40 |
| Sunshine/Moonlight, earlier reference | 120/120 | 28.05 / 40.44 / 58.18 | 16.44 / 24.51 / 38.20 | 58.62 | 16.19 / 24.00 / 33.68 | 0 | 45.35 / 46.27 | 260.29 / 269.81 | 28.94 |

Discarded experiments, not included in the final code:

| Experiment | Inputs | Input p50/p95/p99 ms | Video p50/p95/p99 ms | Unique fps | Frame interval p50/p95/p99 ms | Stalls >100ms | CPU idle/motion % | RSS idle/motion MiB | Runtime MiB |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Direct RTP elapsed timestamps | 120/120 | 56.03 / 76.44 / 84.37 | 52.26 / 68.83 / 72.10 | 55.73 | 16.05 / 31.99 / 36.05 | 0 | 0.59 / 40.32 | 43.30 / 55.12 | 15.41 |
| OpenH264 single thread | 120/120 | 44.05 / 56.08 / 56.29 | 36.69 / 39.82 / 45.81 | 57.15 | 16.03 / 28.01 / 35.99 | 0 | 1.09 / 43.03 | 43.36 / 55.03 | 15.40 |

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

| Segment | p50 ms | p95 ms |
| --- | ---: | ---: |
| Source draw → damage received | 1.737 | 2.447 |
| Damage received → capture start | 0.036 | 0.253 |
| Capture start → end | 1.682 | 2.605 |
| Capture end → encode start | 8.744 | 15.533 |
| Encode start → end | 4.345 | 6.489 |
| Encode end → RTP write start | 0.137 | 0.391 |
| RTP write start → end | 0.042 | 0.064 |
| RTP write end → rVFC receiveTime | 0.230 | 0.644 |
| receiveTime → presentationTime | 0.700 | 3.600 |
| Decode processingDuration | 0.400 | 0.700 |
| presentationTime → expectedDisplayTime | 19.200 | 31.100 |
| expectedDisplayTime → first client pixel | -9.917 | 9.360 |
| presentationTime → first client pixel | 5.267 | 13.275 |
| Source draw → first client pixel | 25.033 | 32.803 |
| Client XTest → viewer DOM keydown | 0.729 | 3.811 |
| Viewer DOM → datachannel send | 0.100 | 0.200 |
| Datachannel send → agent receive | 0.205 | 0.672 |
| Agent receive → inject start | 0.052 | 0.336 |
| Inject start → end | 0.165 | 0.693 |
| Inject end → source DOM | 0.270 | 0.773 |
| Source DOM → repaint | 7.900 | 16.100 |
| XTest → source repaint | 9.469 | 17.958 |

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

| Run / disposition | Input p50/p95/p99 ms | Video p50/p95/p99 ms | FPS | CPU idle/motion % | RSS idle/motion MiB | Stalls >100ms |
| --- | --- | --- | ---: | --- | --- | ---: |
| Instrumented baseline | 35.89 / 44.29 / 48.07 | 24.98 / 33.16 / 46.00 | 59.51 | 0.68 / 40.46 | 43.38 / 54.90 | 0 |
| Phase-stable deadlines; discarded | 44.05 / 76.19 / 88.22 | 56.20 / 64.51 / 76.24 | 59.67 | 0.50 / 41.86 | 43.23 / 54.49 | 1 |
| Bounded one-frame credit; discarded | 40.08 / 60.25 / 64.71 | 38.94 / 44.26 / 49.45 | 59.76 | 0.60 / 42.56 | 43.67 / 54.93 | 0 |
| Final, original pacer, tracing off | 32.24 / 48.05 / 55.12 | 22.93 / 31.11 / 37.39 | 59.52 | 0.60 / 39.21 | 43.42 / 55.03 | 0 |
| Earlier Moonlight reference | 28.05 / 40.44 / 58.18 | 16.44 / 24.51 / 38.20 | 58.62 | 45.35 / 46.27 | 260.29 / 269.81 | 0 |

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
