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
