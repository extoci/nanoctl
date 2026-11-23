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
