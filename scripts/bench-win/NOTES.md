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
