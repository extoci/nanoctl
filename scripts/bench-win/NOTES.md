# Windows G2G benchmark state (devin/g2g-bench)

## Measured (clock-marker, same VM, 1600x1200)

| path                                               | mean | p50 | p90 | p99 | min | n    |
| -------------------------------------------------- | ---- | --- | --- | --- | --- | ---- |
| Sunshine+Moonlight baseline                        | 19.4 | 17  | 34  | 41  |     | 883  |
| nanoctl baseline                                   | 48.4 | 51  | 68  | 85  |     |      |
| nanoctl slice-parallel + parallel RGBA->I420 (RTP) | 29.7 | 34  | 38  | 53  |     | 1760 |
| nanoctl DC-video + canvas overlay                  | 93.7 | 91  | 113 | 199 | 34  | 981  |
| nanoctl DC-video + MediaStreamTrackGenerator       | 29.5 | 34  | 36  | 49  | 0   | 1779 |
| + sender pacing + reorder buffer                   | 29.8 | 34  | 37  | 47  | 16  | 1740 |
| + SIZELIMITED@1.5KB slices (post-merge re-run)     | 34.0 | 34  | 34  | 37  | 33  | 903  |

Key findings:

- Chrome's receiver-side encoded transform sits AFTER its jitter buffer, so
  tapping RTP frames cannot beat ~20-35ms of target delay. Video over an
  unordered/unreliable RTCDataChannel bypasses it entirely.
- Software canvas `drawImage` on this GPU-less VM costs ~60ms of composite
  time; `MediaStreamTrackGenerator` (the generator IS the track in Chrome
  156: `new MediaStreamTrackGenerator({kind:"video"})`, write VideoFrames to
  `.writable`) presents through the composited video layer instead.
- DC wire format: `[u32 seq][u8 flags(bit0=IDR)][u16 part][u16 parts]
[u64 sent_ms][AU chunk]`, chunk <= 48KiB (SCTP rejects >~64KiB messages —
  IDRs need self-fragmentation). Deltas ride `nanoctl.video.v1`
  (ordered:false, maxRetransmits:0); IDRs ride reliable `nanoctl.video-idr.v1`.
- Resync: seq gap -> drop deltas -> send "KF" on the channel (250ms
  throttle) -> agent `keyframe_requests` counter -> producer
  `force_intra_frame` -> IDR on the reliable channel.
- DC transit send->onmessage ~14ms avg (agent->Chrome on loopback).

## Loss robustness (post-E2E fixes)

- The unreliable delta channel both loses AND reorders packets (~38% of
  messages arrived out of order under load). Silent drops of out-of-order
  frames corrupted the reference chain without a seq gap -> garbage
  decode until the next KF.
- Viewer: reorder buffer (window 16) holds out-of-order deltas and drains
  in seq order; an IDR jumps the buffer (delivered even with a gap ahead)
  so waitingForKey cannot deadlock behind a permanently lost delta.
- Agent: IDRs are paced on the reliable channel (up to 30ms wait per part
  while buffered_amount > 128KiB) so a 3-part IDR burst doesn't starve
  the SCTP cwnd; deltas are skipped when buffered_amount > 64KiB instead
  of queueing stale frames (dc_backed_up flag + dc_skips counter).
- Result: 60s stress with viewer maximized -> 0 gaps, 0 KF requests,
  ~56fps fed, transit ~3ms. Before the fix the same stress produced
  ~4-5 KF/s (lost IDR parts -> resync loop).

## Resources (same VM, 60s samples, 1600x1200)

| process                           | CPU (% of 8 cores) | RSS avg           |
| --------------------------------- | ------------------ | ----------------- |
| nanoctl agent (server)            | 20.2-21.8          | ~99 MB (peak 109) |
| Sunshine (server)                 | 21.3               | ~112 MB           |
| Moonlight (client)                | 43.0               | ~147 MB           |
| nanoctl viewer (Chrome, 10 procs) | —                  | ~400 MB           |

5-min sustained sample (calmer scene): agent 11.6% CPU, RSS avg 103 MB,
peak 158 MB.

Agent == Sunshine in CPU/RSS. Client side: Chrome viewer is ~2.7x
Moonlight's RSS (browser overhead — a Tauri/egui client would erase it);
Moonlight's native client burns 43% CPU vs agent's ~21%.

## Bandwidth (same recursive-mirror scene, 1600x1200)

| stream             | wire rate                                                      |
| ------------------ | -------------------------------------------------------------- |
| Sunshine+Moonlight | 14.99 Mbps (Sunshine log: configured streaming bitrate)        |
| nanoctl DC-video   | ~1.3 Mbps (agent bytes_sent/send_kbps, VBR — content-adaptive) |

~11.5x less wire data. Caveat: Moonlight's figure is its configured
target bitrate — Sunshine pads toward it; nanoctl's is measured output
on a mostly-static desktop, so the gap shrinks on high-motion content.

## Rate-vs-quality frontier (`media-smoke --psnr`, same scene)

`media-smoke --psnr` decodes every access unit in-process and reports
Y+U+V PSNR vs the I420 the encoder consumed (p05 = worst tail). Measured
the QP-floor frontier at 1600x1200 responsiveness, 20s each:

| qp_min           | kbps    | PSNR p50 | PSNR p05 |
| ---------------- | ------- | -------- | -------- |
| 14 (was default) | 606     | 50.8     | 49.2     |
| 22               | 486     | 49.9     | 48.5     |
| 24               | 416     | 48.5     | 47.2     |
| 26               | 363-370 | 47.1     | 46.2     |
| 30               | 373     | 45.0     | 45.0     |

qp_min=26 ships for Responsiveness: **-39% bits** (606->370kbps) while
the worst tail stays >=46dB — well above the ~40dB transparent threshold;
the floor only binds on easy scenes, which is exactly where bits are
wasted. Encode time unchanged (~7ms p50). Also swept and rejected:
usage=screen (+6x encode cost), AQ/BGD/SCD off (bitrate balloons 2.4x),
threads 8 / slice 800 (noise).

## Motion frontier (`benchwin band --motion --height 1200`, scroll scene)

The band's `--motion` mode scrolls a 16px checkerboard 4px per paint tick
across the full display — a uniform pan that motion estimation should
predict, i.e. the gaming-like case (30s `media-smoke --psnr` runs):

| config                     | kbps   | PSNR p50 | PSNR p05 | encode p50 |
| -------------------------- | ------ | -------- | -------- | ---------- |
| Responsiveness (Low, qp26) | 315    | 51.8     | 51.6     | 6.4ms      |
| Complexity::Medium, qp26   | 312    | 50.8     | 50.6     | 6.5ms      |
| qp_min 30                  | 334    | 51.4     | 50.7     | 6.5ms      |
| qp_min 34                  | 264    | 42.8     | 42.8     | 6.4ms      |
| **SCD off (ships)**        | **83** | **59.0** | **57.9** | **5.3ms**  |

`scene_change_detect` fired ~15 IDRs per 30s on the scrolling scene —
each reset the reference chain at intra cost and dipped quality. Off:
**-74% bits and +7dB under motion** (315->83kbps, 51.8->59.0 PSNR,
encode 6.4->5.3ms), and the static scene is unchanged-or-better
(116->103kbps, 1 IDR in 30s, PSNR 45.85 flat). Loss recovery is
unaffected — forced keyframes ride the existing KF-request path; a real
scene change just encodes as a large delta. The earlier "2.4x bitrate"
rejection covered AQ/BGD/SCD all off together — AQ-off was the culprit.

## Join latency (channel attach -> first keyframe fed to the decoder)

`__dcv.joinMs` instruments the DC-video attach-to-first-keyframe window
(`webcodecs-video.ts`). On loopback, n=5 per variant:

| variant                              | joinMs (n=5)        |
| ------------------------------------ | ------------------- |
| baseline (KF sent after first delta) | 103.6 / 105 / 134.8 |
| KF sent immediately at attach        | 89.1-134, med ~104  |

Loopback gain is ~10ms median — the bigger win is off-box: the KF request
otherwise waits for the first delta to arrive, which costs at least one
RTT before the encoder even starts the IDR (and longer when the producer
is idle). Agent-side proactive IDR on channel open was tried and reverted:
it raced the viewer's own KF and emitted a duplicate ~150KB IDR that
congested SCTP (+30-50ms joinMs).

**Stream replay (ships).** The writer keeps a process-global, ~4MB replay
window of the last complete keyframe plus every delta after it. A 5ms
attach poll snapshots it and replays the tail over the reliable channel —
ordered delivery is required, an unordered delta outrunning the keyframe
makes the viewer drop it as "late" — with seq renumbered into the live
stream. The attach-time `KF` is swallowed for 1s after a served replay so
no duplicate IDR follows.

| variant                           | joinMs (n=3)    |
| --------------------------------- | --------------- |
| cold attach (empty replay window) | ~127            |
| **reattach (replayed tail)**      | **10.5 / 16.4** |

~8x faster rejoins. The first-ever attach still waits for a fresh IDR
because nothing has been encoded yet.

**Replay warmup (ships).** At agent start `prime_replay` does a single-shot
`Monitor::capture_image` + one H.264 IDR encode and atomically `seed`s it
into the replay window (seed refuses once any live keyframe exists, so it
can never corrupt the delta chain). A session-connect retry covers agents
whose boot capture failed. A second _recorder_ is not used: an early
version spawned `CaptureEncoder::primary`, whose capture session starved
the real pipeline's recorder and timed out its first-frame wait at 5s.

| variant                             | joinMs (n=3)  |
| ----------------------------------- | ------------- |
| **first-ever attach (warm window)** | **4.5 / 6.5** |

The warm frame can be seconds-to-minutes old on the very first attach; the
pipeline's own startup IDR replaces it ~50ms later. Encoder smoke
unchanged: 5.65ms p50, PSNR p05 45.85dB, ~108kbps static.

**Attach→first decode** (`__dcv.firstDecodeMs`): **14–22ms** — join ~9ms plus
~5–12ms software decode+present on this GPU-less VM.

**srcObject race fix (ships).** `peer.ontrack` unconditionally overwrote
`video.srcObject` with the RTP stream; if it fired after the DC path had
claimed the element, the MediaStreamTrackGenerator went unconsumed,
`writer.write()` pended, unclosed VideoFrames wedged Chrome's decoder and
the viewer stayed on "waiting for video" forever. Now the RTP stream is
only assigned when no DC path is attached (the RTP fallback is preserved —
`restoreRemoteVideo` puts it back on teardown).

## Delivered fps under motion (`__dcv` counters, 30s windows)

End-to-end decode delivery while the scrolling checkerboard covers the
full display. The painter originally cost ~7k FillRects/frame and capped
the scene at 17.6fps — pre-rendering the checkerboard to a memory bitmap
(one BitBlt/frame) restored the composite rate:

| delivered | decoded | decodeDropped | gaps | kf  |
| --------- | ------- | ------------- | ---- | --- |
| 29.4 fps  | 29.4    | 0             | 0    | 0   |

Every composite tick is delivered and decoded (~30Hz ceiling of the
IddSample display); the software decoder keeps up with zero queue drops.

## Input-to-photon (click through the viewer)

benchwin's band counts WM_LBUTTONDOWN in its marker; clicking the streamed
band through the viewer produced an increment visible in the client's
displayed frame at lag 34ms — same pipeline lag as G2G, plus <=1 band
repaint (~35-50ms total on this composite-bound VM).

## Server per-frame costs (agent.log pipeline statistics, /frames_encoded)

- backend_ns (OpenH264): was ~25ms -> ~10ms (max_slice_len 8000 +
  num_threads 4), then ~5.6ms with max_slice_len 1500 (SIZELIMITED
  slices 6-9KB deltas across encode threads — the earlier
  SM_FIXEDSLCNUM_SLICE writeback via SetOption(SVC_ENCODE_PARAM_EXT)
  reset encoder state and forced an IDR; reverted to a small size cap)
- color_convert_ns (RGBA->I420): was ~4.5ms -> ~1.3ms (4-thread row bands)
- capture_wait_ns ~6.4-11ms; send_ns ~0.2ms; compare_ns ~0.01ms
- post-merge G2G samples quantize to ~33-37ms == one composite period:
  the IddSample virtual display composites at ~30Hz, so capture never
  returns pixels newer than the last tick; ~34ms is the VM's floor

## Honest ceiling on this VM

No GPU (IddSampleDriver virtual display + Basic Display Adapter), composite
~30Hz -> ~33ms is the capture-side floor as well as the presentation floor;
the latest run sits on it (mean 34.0, min 33). Moonlight wins
here mainly via its native client (no browser) + NVENC on real hardware.
On a GPU box the DC path should pull ahead: decode+presentation become
hardware and the jitter-buffer bypass is worth ~20-35ms on its own.

## Env notes

- bench processes: `nanoctl.exe run --config C:\Users\Administrator\bench\nanoctl.toml`
  via `C:\Users\Administrator\bench\start-agent.ps1`; Chrome viewer
  --remote-debugging-port=9222 ("devinbox · nanoctl"); benchwin band
  --clock; kill Sunshine/Moonlight before nanoctl measurements; move the
  agent console off the viewer with `benchwin move "nanoctl.exe" 600 500 900 400`.
- The agent serves the viewer embedded at build time (build.rs packs
  apps/viewer/dist) — rebuild the agent after rebuilding the viewer.
- Debug counters on the page: `window.__dcv` {msgs,fed,gaps,kf,transitMs,
  decoded,decodeDropped,waitingForKey,late,reordered}.
- Window occlusion breaks the marker scan (locate -> marker_y=-1): the
  viewer window must be visible and not maximized over the band. If it
  paints white after restore, `Page.reload` via CDP :9222.

## Frame-slot queue (cycle 9)

`capture_to_encode` decomposes: `queue` (capture → encode dispatch) +
`encode`. media-smoke now reports `queue_p50_milliseconds`: 7.85ms in the
harness (its 60Hz pull cadence parks ~30Hz frames half an interval), vs
**0.91ms** queue_ns/frame in the live producer. Real production
capture→encode ≈ 11.6ms = backend 7.5 + color 2.3 + compare 0.9 + queue 0.9.
Also removed a redundant full-frame `differs_from` after every changed
encode (~0.87ms × encoded frames ≈ 1.5% agent CPU under motion).

## RGBA→I420 conversion (cycle 10)

Replaced hand-rolled 4-thread scalar conversion with the `yuv` crate's
runtime-dispatched SIMD (SSE→AVX2), same BT.601 limited matrix. 720p
microbench: 1103µs → 189µs/frame (−83%). media-smoke encode p50
5.43→4.07ms (−25%), capture_to_encode 13.1→11.79ms. Bit-exact test
relaxed to ±1 LSB — SIMD rounds chroma slightly differently; PSNR
unchanged (45.9dB).

## Motion-scene fidelity + worst-case encode (cycle 11)

The ~330-900kbps media-smoke numbers seen under the scrolling
checkerboard were NOT a capture bug — they are correct encoder output.
AU-size instrumentation (`au_p50_bytes`/`au_max_bytes`, plus
`fresh_frames` counting new-vs-cached pulls) and frame dumps
(`NANOCTL_DUMP_FRAME=<path>` env on Windows) show:

- 4px/tick uniform BitBlt scroll: ~650B deltas — OpenH264 ME codes the
  whole zone as P_SKIP + one motion vector. Encoder doing its job.
- Random flat 64px cells: ~1.1KB deltas — flat MBs code cheap.
- Full-zone black<->white flash (`benchwin band --flash`): ~4.7KB deltas.
- Per-pixel random noise (`benchwin band --noise`, two alternating DIB
  fields): au_p50 44KB, **20.1 Mbps**, encode p50 **16.5ms**, p95 24ms —
  the honest worst case; the 24 Mbps cap and the 60fps budget both bind.
  Threads 4/6/8 identical (16.4-16.5ms) — CAVLC entropy coding is the
  serial wall; slice-parallelism does not scale there.
  `NANOCTL_ENCODE_THREADS=N` env overrides the encoder thread count for
  future A/Bs; default stays 4.

benchwin extras: `band --flash`/`--noise` scenes, `grab <png>` (WGC
single-shot), `recgrab N` (stream-recorder content hashes + a PNG) —
probes used to prove capture sees what the screen shows.
Viewer: `__dcv` now exposes decode (submit->output) and present
(output->composited rVFC) sums/max via `decodeSum/N/Max` and
`presentSum/N/Max`.

## x264 backend on Windows (cycle 12)

The in-tree `x264` feature now builds on Windows: `build.rs` accepts
`NANOCTL_X264_INCLUDE=<dir with x264.h>` when pkg-config is absent, and
the shim dlopens `libx264-165.dll` (MSYS2 `mingw-w64-x86_64-libx264`
package) placed beside nanoctl.exe. Shim gains env knobs:
`NANOCTL_X264_PRESET` (default `superfast`), `NANOCTL_X264_TUNE`
(default `zerolatency`), `NANOCTL_X264_THREADS` (default 2, sliced).

Same media-smoke protocol, OpenH264 -> x264(superfast):

| scene              | fps                 | encode p50           | bitrate                                                               | PSNR p50       | verdict      |
| ------------------ | ------------------- | -------------------- | --------------------------------------------------------------------- | -------------- | ------------ |
| noise (worst case) | 39.6 -> 58.0 (+46%) | 16.9 -> 7.5ms (-56%) | 15.5 -> 11.2Mbps (-28%)                                               | 25.3 -> 24.2dB | fail -> pass |
| motion             | 58 -> 58.2          | ~7 -> 3.8ms          | 462kbps (vs 618 ultrafast)                                            | 60.8dB         | pass         |
| flash              | -                   | 4.2ms                | 633kbps                                                               | 63.2dB         | pass         |
| static             | 58                  | 3.3ms                | 666kbps (CRF12 encodes every frame; OpenH264 emitted ~61B P-skip AUs) | 68dB           | pass         |

Preset sweep (noise): ultrafast 6.2ms/11.3Mbps/24.5dB, superfast
7.5ms/11.2/24.2, veryfast 11.5ms/10.1/24.2 (fps drops to 52). On motion
superfast saves 25% bits vs ultrafast (618->463kbps, 61dB still
transparent) -> shipped as default. Threads=4 under noise: enc -19% but
bitrate +75% (slice boundaries kill compression) -> stays 2.

Caveats: noise PSNR is CRF-quality-bound (~24dB either encoder; content
is uncompressible, quality trade is the honest one). Shipping
libx264.dll is GPL — packaging decision deferred.

## x264 CRF sweep (cycle 13)

`NANOCTL_X264_CRF` env added to the shim; default `f_rf_constant`
12.0 -> **18.0** (overridable per-session). media-smoke sweep on the
motion scene:

| crf | kbps           | PSNR p50 / p05 |
| --- | -------------- | -------------- |
| 12  | 516            | 60.0 / 59.2    |
| 15  | 583            | 58.8 / 58.2    |
| 18  | 332-336 (-36%) | 56.5 / 56.1    |
| 21  | 315 (-39%)     | 53.2 / 52.9    |

Regression check at crf18: noise 57.5fps/11.0Mbps/24.3dB (unchanged —
pure noise is VBV-bound, not CRF-bound), flash 58.1fps/562kbps/58.3dB,
static **666->70kbps (-90%)** at 55.9dB. All p05 >= 55.9dB except noise
(content-bound, identical at every CRF). 18 chosen over 21: nearly all
the savings, +3dB margin.

## Viewer client-path breakdown + motion G2G (cycle 14)

CDP probes (now in this directory): `g2g-probe.ts` (in-page clock-marker
G2G), `dcv-snap.ts` (`window.__dcv` decode/present/transit counters),
`join-bench.ts` (joinMs/firstDecodeMs). Chrome runs with
`--remote-debugging-port=9222`; page ids from `localhost:9222/json/list`.

Measured under `benchwin band --motion` (1600x1200, 58.6fps delivered,
0 gaps, 0 decodeDropped, 15s window):

- decode submit->output: **1.34ms avg**
- decode output -> composited rVFC present: **11.15ms avg**
- wire transit (send->recv): **~2.0ms avg**

In-page G2G under `--clock --motion` (25s, n=395): **mean 56.9, p50 57,
p90 72, p99 80** vs ~34ms semi-static. Pipeline stages account for
~26ms; the ~31ms remainder is capture-side staleness — under continuous
repaint the WGC-delivered frame is roughly one ~30Hz composite period
old before encode starts (xcap WGC uses a rendezvous channel with a
2-buffer free-threaded pool; FrameArrived delivers the newest frame, so
the lag is WGC/compositor pacing, not consumer queue buildup).
Present wait is likewise vsync/composite-bound. Both are floor effects
of this GPU-less VM; on a real display at 144Hz both shrink with the
composite period.

## Agent CPU + RSS: OpenH264 vs x264 (cycle 15)

Same media-smoke run on the same VM under `band --noise` (worst-case
content), backend toggled via `NANOCTL_SOFTWARE_ENCODER=openh264`:

| metric                 | OpenH264 | x264     | delta |
| ---------------------- | -------- | -------- | ----- |
| agent CPU (of 8 cores) | 19.6%    | 13.0%    | -34%  |
| peak RSS               | 243MB    | 113MB    | -53%  |
| delivered fps          | 51.9     | 58.4     | +12%  |
| encode p50             | 16.8ms   | 7.0ms    | -58%  |
| bitrate                | 20.6Mbps | 11.2Mbps | -46%  |

(Bitrate/fps vary run to run on the noise scene - direction is stable.)

## x264 preset re-sweep: superfast -> veryfast (cycle 16)

Motion-scene preset sweep at CRF18 (media-smoke, 12s):

| preset    | kbps       | encode p50 | PSNR p50 |
| --------- | ---------- | ---------- | -------- |
| superfast | 355        | 3.7ms      | 56.8dB   |
| veryfast  | 235 (-34%) | 4.8ms      | 57.4dB   |
| faster    | 324        | 6.1ms      | 57.8dB   |

Default moved superfast -> veryfast: -34% bitstream on realistic
content, slightly better PSNR, +1.1ms encode. Noise worst-case stays
green: 54.8fps, encode 10.4ms, 10.8Mbps, 24.3dB (passes; fps dips vs
58 under superfast - the honest trade). `NANOCTL_X264_PRESET` still
overrides.

## Viewer client resources: Chrome viewer vs Moonlight client (cycle 17)

Live motion stream (~32fps delivered, 1600x1200), whole-process sums:

| client                                | CPU (8-core %) | RSS    |
| ------------------------------------- | -------------- | ------ |
| nanoctl Chrome viewer (bench profile) | 4.0            | 466MB  |
| Moonlight client (earlier measure)    | ~43            | ~147MB |

~10x lower client CPU than Moonlight (WebCodecs decode +
MediaStreamTrackGenerator compositing is nearly free); 3x RSS — the
browser tax a native client would erase. In-stream path under motion:
transit 1.5ms, decode 1.16ms, present 12.4ms, 0 gaps/drops.

## x264 profile sweep: baseline vs main/high (cycle 18)

`NANOCTL_X264_PROFILE` env knob added (default `baseline`, i.e.
CAVLC). CABAC profiles win big on realistic content but lose under
worst-case noise:

| scene  | profile            | kbps       | fps       | encode p50 | PSNR p50 |
| ------ | ------------------ | ---------- | --------- | ---------- | -------- |
| motion | baseline (default) | 235        | ~57       | 4.8ms      | 57.4dB   |
| motion | high               | 170 (-28%) | 57.9      | 5.0ms      | 57.6dB   |
| noise  | baseline           | 10,800     | 54.8      | 10.4ms     | 24.3dB   |
| noise  | high               | 7,149      | 35.8 FAIL | ~14ms      | 24.2dB   |
| noise  | main               | 7,440      | 37.3 FAIL | -          | 24.2dB   |
| noise  | high+superfast     | 8,373      | 43.0 FAIL | -          | 24.1dB   |

CABAC entropy coding under per-pixel noise costs ~35-40% fps even at
superfast - unacceptable for particle-heavy game content, so baseline
stays default. `-28%` bits available via env on CABAC-capable content.

## x264 motion-estimation sweep: hex vs umh (cycle 19)

`NANOCTL_X264_ME` env knob added (dia/hex/umh/esa/tesa; default hex,
the veryfast default). Better ME finds cheaper motion vectors:

| scene  | me            | kbps         | fps  | encode p50 | PSNR   |
| ------ | ------------- | ------------ | ---- | ---------- | ------ |
| motion | hex (default) | 235          | ~57  | 4.8ms      | 57.4dB |
| motion | umh           | 194 (-17%)   | 58.3 | ~5ms       | 57.4dB |
| noise  | hex           | 10,800       | 54.8 | 10.4ms     | 24.3dB |
| noise  | umh           | 9,501 (-12%) | 47.9 | 14.3ms     | 24.0dB |

umh passes the noise worst-case but costs -13% fps and +3.9ms encode
there - the ME search gets expensive exactly when MVs are random.
Default stays hex; umh available via env when encode headroom exists.

## Audio path: un-benchmarkable on this VM

`system_audio` streams Opus LowDelay 48kHz stereo @128kbps via WASAPI
loopback, but this headless box has no audio endpoint and the Windows
Audio service cannot start (no render device). Audio latency/bitrate
numbers need a box with a sound device (or a virtual audio driver).

## Attach churn bench (cycle 20)

`churn-bench.ts <page-id> [N]` reloads the viewer page N times via CDP
and reports joinMs/firstDecodeMs/live/decoded/kf/gaps per attach - a
regression probe for the replay-window + decoder-attach path.

8/8 attaches clean under motion scene: joinMs 4.6-10.9 (mean ~8),
firstDecodeMs 8.3-16.5, exactly 1 kf each, 0 gaps, all live+decoding.
No wedge/leak across repeated attach/detach churn.

## x264 subpel-refine sweep (cycle 21)

`NANOCTL_X264_SUBME` env knob added (`p.analyse.i_subpel_refine`,
default 2 = veryfast). Worst-case fps vs bitrate trade:

| scene  | subme       | kbps           | fps        | encode p50   | PSNR p05 |
| ------ | ----------- | -------------- | ---------- | ------------ | -------- |
| noise  | 1           | 11,190 (+3.7%) | 58.3 (+6%) | 7.7ms (-25%) | 24.0dB   |
| noise  | 2 (default) | 11,121         | 56.2       | 10.2ms       | 24.1dB   |
| noise  | 3           | 10,601         | 53.9       | 11.5ms       | 24.1dB   |
| motion | 1           | 319 (+34%)     | 58.2       | 3.6ms        | 56.4dB   |
| motion | 2 (default) | 238            | 58.3       | 4.4ms        | 56.8dB   |

subme=1 wins +6% worst-case fps but pays +34% bits on realistic
motion - keeps the cheap-MV cost where it hurts most. Default 2.

## Sustained-load soak: 60s worst-case noise (cycle 22)

media-smoke, per-pixel noise scene, 60s continuous: 3,498 frames @
58.3fps, 11.5Mbps, encode p50 10.6ms / p95 12.9ms, passed.
Encoder process RSS flat at ~91-96MB across the whole run - no growth
or degradation under a full minute of worst-case load.

## x264 adaptive-quantizer sweep (cycle 23)

`NANOCTL_X264_AQ` env knob added (`mode[:strength]` on
`p.rc.i_aq_mode`/`f_aq_strength`; default = veryfast's aq_mode 1 @
1.0). Motion scene: AQ is doing massive bit shaping - the defaults are
already at the pareto point.

| aq                | kbps        | PSNR p05 | encode p50 |
| ----------------- | ----------- | -------- | ---------- |
| off (0)           | 789 (+231%) | 62.8dB   | 4.5ms      |
| 1 @ 1.0 (default) | 238         | 56.8dB   | 4.4ms      |
| 1 @ 1.5           | 280 (+18%)  | 53.8dB   | 4.5ms      |

## Plan: adaptive encoder negotiation (user directive, cycle 24+)

"Best possible defaults ever, no compromises" = per-content params at
runtime, not one static tuple. The sweeps define the policy table:

- normal content (bits << VBV cap, encode fits budget): PROMOTE -
  high profile (CABAC, -28%) + umh (-17%) costs ~+1ms encode there.
- stress content (bits near cap OR encode_p50 > ~14ms): DEMOTE -
  baseline/hex/subme2 (worst-case-safe: 54-58fps noise).
- Mechanism: x264_encoder_reconfig for CRF/AQ/VBV; profile/ME changes
  need encoder re-open + forced IDR (SPS switch in-band, decoder-safe
  since deltas can't cross a re-open anyway).
- Classifier inputs already on the loop: bits/AU window, encode_ns,
  qp trend, frame-changed fraction. No extra capture pass needed.
- Debugging needed first: per-second time-series (fps/kbps/encode_ms)
  in media-smoke JSON + effective-param echo, so adaptation is
  observable and re-sweeps stay honest.

## Adaptive encoder negotiation (cycle 24-25)

Shipped: content-driven parameter negotiation for the x264 backend.
`AdaptPolicy` (in `x264_encoder.rs`) watches per-frame encode_ns and AU
bytes, promotes after ~150 easy frames under 30% encode budget / 50%
VBV, demotes after ~20 stressed frames, drops straight to tier 0 on
hard overload. Tier switches re-open the encoder (profile/ME can't
reconfig) + force an IDR. Viewer `webcodecs-video.ts` re-configures
the decoder when the in-band SPS profile changes between IDRs.
Kill-switch: `NANOCTL_X264_ADAPT=0`; any env-pinned knob
(PROFILE/ME/SUBME) is excluded from adaptation.

| tier | params                   | when                  |
| ---- | ------------------------ | --------------------- |
| 0    | baseline / hex / subme 2 | default + stressed    |
| 1    | + high (CABAC, 8x8)      | sustained easy window |
| 2    | + umh                    | still easy at tier 1  |

Measured (15s smokes; per-second tier in JSON makes it observable):

- motion: promotes 0->1 @ ~4s, 1->2 @ ~7s, holds. Steady tier-2
  seconds ~135kbps vs ~235 tier-0 (-42%), fps 58.2, encode p50
  4.6ms, PSNR p05 56.5dB - same quality, ~2 extra IDRs total.
- noise (worst case): stays tier 0, 0 switches, 55fps, 10.9Mbps,
  encode p50 10.4ms - identical to the fixed baseline, zero
  regression by construction (same params, EWMA overhead only).

Observability: `x264_tier` per-second field + `x264_adapt_tier` /
`x264_adapt_switches` top-level in smoke JSON; `tracing::info!` on
every switch.

## NR / scenecut / fast-path sweep on noise (cycle 26)

New env knobs `NANOCTL_X264_NR` (x264 `analyse.i_noise_reduction`) and
`NANOCTL_X264_SCENECUT` (`i_scenecut_threshold`, default stays 0).
Worst-case noise, 15s smokes (baseline: 53.8fps, 10.6Mbps, encode
10.6ms, PSNR p05 24.07dB, tier 0 / 0 switches):

| env             | fps  | Mbps  | enc ms | p05 dB |
| --------------- | ---- | ----- | ------ | ------ |
| baseline        | 53.8 | 10.62 | 10.57  | 24.07  |
| NR=150          | 53.6 | 10.60 | 10.56  | 24.08  |
| NR=300          | 54.3 | 10.73 | 10.38  | 24.07  |
| NR=600          | 52.9 | 10.42 | 10.50  | 24.08  |
| NR=1000         | 53.0 | 10.44 | 10.69  | 24.08  |
| ME=dia          | 55.9 | 11.03 | 9.90   | 24.08  |
| TUNE=fastdecode | 56.9 | 11.11 | 10.20  | 24.02  |

Rejected — all flat or +4%-bits-for-+4%-fps trades: under noise the
encoder is VBV-capped (CRF unreachable), so denoising has no bits to
recover and cheaper analysis only buys fps at the cap's bitrate.
Flash scene sanity: 57fps / 292kbps / 57.5dB, adapt promotes
correctly to tier 1 between flashes.

## Adaptive tier 3: slower preset on easy content (cycle 27)

Tier ladder extended: 0 baseline/hex/subme2, 1 +high, 2 +umh, 3 =
preset `fast` + subme 6 (preset env-pinned via `NANOCTL_X264_PRESET`
masks it like the other knobs). Policy `MAX_TIER` 2->3; promotion
still requires <30% encode budget and <50% VBV for ~150 frames, so
tier 3 only engages on easy content and demotes on any stress.

Measured (20s motion smoke): 0->1 @ ~2s, ->2 @ ~5s, ->3 @ ~8s, holds

- steady tier-3 ~128-133kbps vs ~135 tier-2 (-4%), PSNR p05 56.5dB,
  fps 57.5. Encode p50 5.78ms overall (fits under the 55% stress
  threshold; anything slower than `fast` would not). Noise: tier 0,
  0 switches, metrics identical - no regression by construction.

## Real-content + live demotion (cycle 28)

ffplay fullscreen on screen during media-smoke (real video instead of
synthetic scenes):

| content                       | tier | fps  | avg kbps | p05 dB |
| ----------------------------- | ---- | ---- | -------- | ------ |
| Big Buck Bunny 720p (cartoon) | 3    | 58.4 | 292      | 56.1   |
| testsrc2 lavfi (dense charts) | 0    | 51.8 | 10258    | 24.05  |

testsrc2 is effectively worst-case: dense per-frame-changing detail
lands within ~4% of the per-pixel-noise numbers, so synthetic noise is
a representative hard bound. BBB promotes to tier 3 and sits at
~240-290kbps steady - real video between the two synthetic extremes.

Mid-stream hard transition (motion 9s -> noise): adapt climbed to
tier 3, then hard-dropped 3->0 inside the first noise second; the
transition second dipped to 50fps (vs 54-56 steady) during the
re-open, then matched pure-noise metrics exactly. Worst-case safety
holds end-to-end, not just in the unit test.

## Faster promotion ramp (cycle 29)

Per-step ease windows `[60, 90, 150]` frames (was a flat 150): tier 1
(CABAC, cheap) engages ~1s in, tier 3 stays careful at ~2.5s of ease.
Motion reaches tier 3 at ~5s vs ~8s (-38% time-to-optimal); steady
state unchanged ~135kbps / 56.6dB / 57.9fps; noise tier 0, 0 switches.

Also swept under noise and rejected: threads=4 (+6% fps, +80% bits,
+0.8dB), AQ modes 1/2/3 (flat - noise is CRF-bound at ~11Mbps against
the 24Mbps VBV cap, not VBV-bound).

## Trellis sweep + latency-mode comparison (cycle 30)

`NANOCTL_X264_TRELLIS` env knob → `analyse.i_trellis` (default 0).
Rejected hard: trellis=1/2 under noise → encode 10.4→30ms, fps
55→25-26 (pipe becomes encode-bound); motion flat (218 vs 216kbps).

Latency-mode A-B (VBV window: Resp=1f, Balanced=2f, Quality=4f):

| mode           | noise kbps | noise fps | noise dB | queue p50 ms | motion kbps | motion dB |
| -------------- | ---------- | --------- | -------- | ------------ | ----------- | --------- |
| responsiveness | 10,620     | 55.2      | 23.9     | ~0.9         | 203         | 56.7      |
| balanced       | 15,113     | 50.3      | 24.7     | 8.5          | 256         | 55.1      |
| quality        | 19,889     | 50.6      | 25.2     | 8.6          | 253         | 55.3      |

Balanced/Quality add ~8.5ms of encode queueing AND 42-88% more bits
for +0.8-1.3dB under noise, and _lower_ motion PSNR. Responsiveness is
the best mode on every axis with the adaptive encoder — the larger VBV
windows just let rate control spend more under CRF-bound content.

## Input→photon decomposition (cycle 31)

`input-photon.ts`: CDP `Input.dispatchMouseEvent` into the viewer page
→ remote DC → SendInput → band `INPUT_COUNT` bump decoded from the
stream. 10 clicks, loopback: **p50 48ms / min 43 / mean 55** while the
same run's clock-marker G2G read p50 61ms — the input lane (reliable DC

- coalesced motion watch-channel) adds ≈0-5ms over the video path;
  input→photon is entirely pipeline-bound, nothing to shave.

## fps decimation disproven + trivial-content tier skip (cycle 32)

`max_fps=30` under noise: 11.57Mbps vs 10.62 at 60fps, PSNR 25.0 vs
23.9 — halving the frame rate does NOT halve bits; x264 refills the
per-frame VBV budget and returns quality instead (+1.1dB). The
fps-degrade lever is dead: it costs fps for ~0 bit savings.

input→photon under noise: p50 49ms / mean 59 (n=8) — same as clean
pipeline; the input lane survives worst-case unchanged.

`AdaptPolicy` promotion can now jump `tier+2` when content is
trivially easy (encode EWMA <12% budget AND bytes <20% cap) — saves
one re-open/IDR burst on attaches to near-idle desktops. Verified as
a no-op on this VM: even a static desktop encodes ~5.5ms/frame
(>12%), so all scenes still promote 0→1→2→3 stepwise; worst case
unchanged (noise tier 0, 0 switches).

## Live wire bytes vs smoke (cycle 33)

`window.__dcv.bytes` added to the viewer (sum of DC message bytes). Measured
live under `band --motion --height 300` on loopback: steady-state wire rate
~179 kbps vs media-smoke encode output ~190 kbps for the same scene — parity
within measurement noise. The initial attach replay burst (~400KB: latest
keyframe + replayed deltas) is one-time and amortizes to nothing on any
session longer than ~30s. Conclusion: smoke numbers are honest wire numbers;
no hidden overhead in the live path.

## Idle + install-size axes (cycle 34)

Measured on this VM (8 logical CPUs):

| Condition                | nanoctl agent                    | Sunshine                       |
| ------------------------ | -------------------------------- | ------------------------------ |
| Idle, no viewer          | 0.0% CPU, pipeline fully stopped | 0.0% CPU, ~31MB RSS            |
| Streaming static desktop | ~8.6% CPU, ~115MB RSS            | ~21% CPU, ~112MB RSS (earlier) |

The streaming-static cost is xcap's Windows pull model (continuous frame
readback at capture cadence; no dirty-rect events — Linux gets XDamage, macOS
ScreenCaptureKit). The producer skips encode on unchanged frames; remaining
cost is capture readback (~4.3ms) + compare (~0.9ms) per poll — platform floor
on this backend.

Install/runtime footprint: nanoctl agent = 20MB exe + 2MB libx264 = **22MB**,
viewer = any browser (0 install). Sunshine = 96MB, Moonlight client = 103MB —
total stack ~199MB vs ~22MB (**~9x lighter**, and the client side needs nothing
installed at all).

Flash-scene adapt verification (same cycle): `band --flash` → tier 0, 0
switches, ~1.1Mbps — policy correctly refuses promotion under byte-spiky
content.

## Sunshine matched-scene A-B (cycle 35)

Moonlight CLI `stream 127.0.0.1 Desktop` against Sunshine on the same
`band --motion` scene (pktmon can't count loopback bytes; figures are
Sunshine's logged wire target + process sampling):

|                  | Sunshine+Moonlight                        | nanoctl                                          |
| ---------------- | ----------------------------------------- | ------------------------------------------------ |
| Encoder          | libx264 software, High L4.2               | x264 software, adaptive tier (baseline→high+UMH) |
| Wire bitrate     | fixed target 14,988 kbps                  | ~190 kbps adaptive (**~79x less**)               |
| Server CPU / RSS | 13.8%, 105MB                              | ~8.6-13%, ~115MB                                 |
| Client CPU / RSS | Moonlight 56.5% (FFmpeg sw decode), 129MB | Chrome viewer ~4%, ~466MB                        |

Sunshine's CBR-ish target means its bitrate is content-independent — the gap
is largest exactly where adaptivity pays (static/motion desktop). Under
worst-case noise nanoctl still sends ~11.2Mbps < 15Mbps. Client-side the
browser viewer costs 1/14th the CPU of Moonlight's software-decode path but
~3.5x the RSS.
||||||| parent of a5aff93 (viewer: open the signaling socket alongside /v1/info)

## Session startup round trips (cycle 36)

`connect-bench.ts <base-url> [N]` (Playwright Chromium, fresh page per run)
timestamps navigation → signaling `WebSocket` created → server hello →
`RTCPeerConnection` connected. RTT modeled with `tc qdisc add dev lo root
netem delay 20ms` (40ms RTT) against a Linux loopback agent, n=15 medians:

| variant                          | ws    | hello | connected |
| -------------------------------- | ----- | ----- | --------- |
| 0ms: `/v1/info` gates the socket | 73.8  | 75.6  | 166.1     |
| 0ms: `/v1/info` alongside socket | 62.1  | 67.5  | 149.2     |
| 40ms RTT: gated                  | 186.1 | 307.8 | 588.9     |
| **40ms RTT: alongside**          | 144.2 | 266.3 | **548.5** |

The hello already carries the host info, so the serialized `/v1/info` fetch
cost exactly one RTT on every connect and reconnect. It still runs (its 403
body is the only verbatim refusal explanation): a socket that fails before
hello waits for its outcome before retrying.

## DXGI dirty-rect capture (cycle 37)

Replaced the xcap DDAPI recorder (full-texture readback + full-frame memcmp
every composite) with a custom `IDXGIOutputDuplication` path that honors
`GetFrameDirtyRects`/`GetFrameMoveRects`: only dirty regions are copied
GPU→staging→CPU, frames emit only when the desktop actually presented
changes, and `backend_changed` carries the backend's own change verdict so
the producer skips the full-frame memcmp entirely. `NANOCTL_DXGI_CAPTURE=0`
restores the xcap path.

A-B, same `band --clock` scene, adapt pinned off (single nanoctl proc):

|               | xcap path      | DXGI dirty-rect |
| ------------- | -------------- | --------------- |
| Agent CPU     | 10.9%          | **6.1% (−44%)** |
| fps / bitrate | 58.2 / 106kbps | 58.0 / 101kbps  |

Worst case (`band --noise`, full-screen dirty every frame): 58.2fps /
19.4Mbps / tier 0 — no regression vs the xcap path. Remaining known cost:
`accum` still clones ~7.7MB per emit (~1% CPU); a generation-tracked buffer
recycle could reclaim it later. On a real GPU box the compositor path is the
same API — dirty rects should help even more where present rates are 144Hz+.

## Emit-buffer recycling (cycle 38)

`LatestFrame` now stashes pixel buffers of fully-dropped `CapturedFrame`s,
tagged by emit generation; the DXGI capture thread reuses them instead of
`accum.clone()`-ing 7.7MB per emit. A recycled buffer emitted `g` gens ago
is brought current by replaying `patch_log` — the swizzled dirty-region
pixels of recent small-dirty emits (dirt >25% of the frame clears the log,
so worst case degenerates to the clone, never worse).

Same `band --clock` A-B: agent CPU **6.1% → 5.6%**; cumulative vs xcap path
**10.9% → 5.6% (−49%)**. fps/bitrate unchanged (58.1 / 108kbps); noise
worst case unchanged (57.3fps / 19.2Mbps / tier 0).

## Cycle 39 — G2G A-B: DXGI dirty-rect emit-on-present vs xcap poll (2026-11-25)

Same rig/scene (`band --clock --height 80 --fps 120`), same agent binary, only `NANOCTL_DXGI_CAPTURE` toggled. g2g-probe, n≈600-1000 per run:

| capture                       | G2G p50  | mean      | p99   | agent CPU (clock scene) |
| ----------------------------- | -------- | --------- | ----- | ----------------------- |
| DXGI dirty-rect (default)     | 41-42 ms | 43.1-43.6 | 78-79 | ~5.6 %                  |
| xcap (NANOCTL_DXGI_CAPTURE=0) | 48 ms    | 48.7      | 73    | ~15.7 %                 |

**DXGI path is also a glass-to-glass win: −6.4 ms p50 vs xcap** — emitting on present beats xcap's fixed-poll cadence. Absolute p50 drifts run-to-run (~34-42 ms across cycles on this ~30 Hz composite VM); only same-rig A-B deltas are comparable.

## Cycle 40 — wire jitter under motion (2026-11-25)

dcv-snap, `band --motion`, 20 s window: 926 deltas, **0 gaps, 0 KF resyncs, 0 decode-drops**; transit avg 1.43 ms, decode avg 1.09 ms (~46 fps consumed). No queueing behind IDR bursts or decode backlog — axis verified clean, nothing to climb.

## Cycle 41 — MMCSS "Pro Audio" thread registration (2026-11-25)

DXGI capture thread, xcap forwarder and encode producer now register with MMCSS `Pro Audio` (`AvSetMmThreadCharacteristicsW` via `Win32_System_Threading`), same trick Sunshine uses for gaming latency scheduling.

G2G A-B, `band --clock`, same rig/session phase (n≈600-1300/run):

|        | p50       | mean     | p99       |
| ------ | --------- | -------- | --------- |
| before | 41-42 ms  | 43.6     | 78-79     |
| MMCSS  | **38 ms** | **39.4** | **63-65** |

**−3.5 ms p50, −14 ms p99** — scheduling-jitter tail mostly gone. Noise fps unchanged (57.8); 60/60 media tests pass.
