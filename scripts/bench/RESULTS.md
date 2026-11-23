# Follow-up: gradient attribution and validated QP14

2026-11-23 UTC. **Retained: QP14–51**, rounded conversion and
`CameraVideoRealTime`; adaptive congestion control is unchanged. No product
latency change or new source-renderer change is retained. The gradient mode
is localized before capture, but neither source mitigation tested here reliably
removes it. The previous three-product campaign below used QP16, not this new
floor: **there is no fresh three-product campaign on QP14**.

## Step 1: gradient source versus damage, encoding, packets and Chrome

Six fresh diagnostic runs used the immutable QP16 release from the preceding
campaign, the same opaque/desynchronized source canvas, agent per-frame traces,
independent source/client XShm marker readers, Chrome rVFC metadata and loopback
UDP captures. Instrumentation adds overhead: these approximately 24/40-ms runs
localize the unprofiled approximately 20/36-ms modes, not their absolute headline
latency. Run video p50s were **39.400, 40.442, 40.176, 36.311, 23.632, 24.471 ms**.

Representative fast run 5 versus slow run 1, per-segment p50 milliseconds:

| Segment                                            |       Fast |       Slow |
| -------------------------------------------------- | ---------: | ---------: |
| Source draw → independently observed source pixels |      5.442 |     18.086 |
| Source draw → damage dequeued                      |      3.749 |     17.218 |
| Damage → capture start                             |       .042 |       .040 |
| Capture start → end                                |      1.433 |      1.561 |
| Capture end → encode start                         |       .184 |       .184 |
| Encode start → end                                 |      6.166 |      6.302 |
| Encode end → sample write start                    |       .210 |       .158 |
| Sample write start → end                           |       .183 |       .190 |
| Sample write end → Chrome receiveTime              |       .296 |       .345 |
| Encode end → first RTP packet                      |       .094 |       .068 |
| First → last RTP packet of a frame                 |       .108 |       .112 |
| Last RTP packet → Chrome receiveTime               |       .486 |       .547 |
| Chrome reported decode duration                    |      1.400 |      1.400 |
| receiveTime → presentationTime                     |      1.800 |      1.800 |
| presentationTime → expectedDisplayTime             |     16.700 |      2.500 |
| presentationTime → detector observation            |      9.208 |     10.854 |
| **Run source draw → detector video p50**           | **23.632** | **39.400** |

There are 1,088/1,068 joined fast/slow frames. **13.47 ms of the 15.77-ms
end-to-end difference is already present at damage dequeue**, while independent
source pixels shift 12.64 ms. This checks against delayed agent damage handling
as the sole explanation. Capture, encode, packet transit and receive→presentation
do not carry a refresh-sized difference. Segment medians need not add to the
end-to-end median. Damage timestamps are dequeue observations, not compositor
completion events. `expectedDisplayTime` is Chrome's prediction, not observed
Xvfb scanout; it must not be interpreted as a measured extra frame queue.

The reachable path is source draw timestamps in `run.ts`, damage/capture and
encoded-marker traces in `media.rs`/`media_trace.rs`, then rVFC marker IDs and
independent detector observations. **The extra delay is localized to source
canvas-to-Xvfb presentation, not proved to be a particular Chromium queue or
vsync implementation.** CPU/scheduling interactions on this shared VM are not
isolated. No product-side pacing or viewer fix is justified by these traces.

### Pacer, RTP bursts and decode counterchecks

`DamagePacer::interval` is 8.333 ms at 60 FPS. `on_frame` uses it as a **minimum
interval between encode admissions**, not a finish-by deadline that adds a
refresh whenever encoding exceeds it. Median encode time is 6.166/6.302 ms;
p95 is 6.717/9.404 ms. Across the complete fast/slow traces, 2.29%/7.60% of
encodes exceed 8.333 ms and .36%/1.46% exceed 16.667 ms. Rare overruns remain,
but cannot explain the dominant delay already observed before capture.

Both runs have **13 RTP packets/frame at p50, 14 at p95**. Burst p95 is
.148/.178 ms; last-packet→receiveTime p95 is .753/.852 ms. Reported decode p95
is 1.5/2.2 ms. Packet timestamps include IPv4 and IPv6 loopback RTP grouped by
SSRC/timestamp and correlated with Chrome metadata; cross-clock alignment uses
agent log timestamps. These are diagnostic software timestamps, not hardware
or optical measurements. **The sender does burst packets**; this loopback result
does not establish acceptable queueing on a bandwidth-limited tailnet. No
transport pacing or congestion-response change was made.

### Source mitigations: rejected after paired validation

Adding `willReadFrequently: true` to the source context failed its diagnostic
pilot: video p50s 24.984, 24.923, 30.955, 41.217 ms. A source-only Chromium
`--disable-gpu-vsync` pilot looked promising (25.270, 23.867, 23.878, 26.414 ms),
but the full paired test disproved its reliability. **Both prototypes were
reverted**; the default remains `{ alpha: false, desynchronized: true }` with
the existing browser arguments. No timestamp origin or workload was changed
to hide the source delay.

Strict fresh-stack CT/TC alternating pairs, identical QP16 binary in both arms;
combined moving-quality/input phases, no instrumentation, builds or full-frame
quality captures during performance measurement. Five gradient pairs and six
drag pairs; medians of all per-run metrics:

| Workload | Source arm        | Input p50 / p95 ms | Video p50 / p95 ms |    FPS |  CPU % |  Mbps |
| -------- | ----------------- | -----------------: | -----------------: | -----: | -----: | ----: |
| Gradient | Control           |    36.035 / 52.389 |    20.505 / 38.135 | 59.724 | 54.614 | 7.136 |
| Gradient | Disable GPU vsync |    40.307 / 52.458 |    33.351 / 37.422 | 59.733 | 51.249 | 7.124 |
| Drag     | Control           |    30.204 / 40.439 |    19.058 / 27.966 | 59.969 | 32.301 | 3.321 |
| Drag     | Disable GPU vsync |    28.470 / 36.583 |    18.958 / 23.229 | 59.951 | 32.190 | 3.322 |

Gradient video p50s, control: **20.168, 34.362, 34.300, 20.505, 20.320**;
candidate: **19.396, 35.269, 23.540, 33.824, 33.351**. Gradient input/video
arm-median deltas **+4.272/+12.846 ms** reject the mitigation. A favorable
instrumented pilot was not enough evidence to retain it.

**21/22 fully valid.** Drag control run 2 matched 119/120 inputs (response 29
unobserved; source received all 120). It remains flagged invalid and included
in the six-run medians; no finite timeout was imputed. The runner initially
stopped, then preserved completed runs and extended drag to six balanced pairs,
giving at least five complete runs per arm without dropping the incomplete run.
Gradient matched 600/600 inputs per arm; drag control/candidate 719/720 and
720/720. All 22 launch inventories passed. Mean/max whole-run VM steal was
1.35%/33% gradient, 1.02%/46% drag; no resource outliers were removed.

## Step 2: QP14 versus the current QP16 control — retained

Five fresh alternating pairs **each** on text and drag, using the unchanged
source default after reverting both prototypes. **20/20 valid**, all 2,400/2,400
inputs observed and all isolation inventories passed. No profiling, extra source
readers, builds or quality-frame analysis during performance phases. The only
encoder difference is minimum QP 16→14; maximum remains 51, including in the
effective-parameter regression for all three latency modes. Rate-control mode,
bitrate feedback, adaptive controller, encoder usage and pacing are unchanged.

| Workload |          Min QP |  PSNR Y dB |  Mbps | Input p50 / p95 ms | Video p50 / p95 ms |  CPU % |
| -------- | --------------: | ---------: | ----: | -----------------: | -----------------: | -----: |
| Text     |      16 control |     50.030 | 2.149 |    28.354 / 36.373 |    18.666 / 20.921 | 31.113 |
| Text     | **14 retained** | **51.213** | 2.249 |    28.447 / 36.391 |    18.844 / 21.239 | 30.526 |
| Drag     |      16 control |     49.366 | 3.317 |    32.029 / 40.232 |    19.056 / 24.051 | 37.549 |
| Drag     | **14 retained** | **50.484** | 3.854 |    31.946 / 39.563 |    19.048 / 22.968 | 35.041 |

| Workload | PSNR gain dB | Input p50 Δ ms | Video p50 Δ ms | CPU Δ points | Gate result |
| -------- | -----------: | -------------: | -------------: | -----------: | ----------- |
| Text     |       +1.183 |          +.093 |          +.178 |        −.587 | Pass        |
| Drag     |       +1.119 |          −.084 |          −.008 |       −2.509 | Pass        |

Both arm-median latency deltas are ≤+2 ms, CPU is ≤control+5 points (and below
51%), and drag is ≤8 Mbps. Median paired deltas also pass. Every individual
pair's positive input/video delta is below +.34 ms. Lower measured CPU is not
claimed as an encoder optimization: whole-run VM steal mean/max was .47%/25%
text and 1.25%/30% drag. These are shared-VM measurements, not confidence bounds.

Retained executable SHA256:
`2d368416c810e7b0dd238208a04f9ebb786e1f6985aa484640730aa95023866e`.
QP14 was **not** benchmarked on gradient or in a new three-product campaign.
Against the previous screen-relay references (text 52.202, drag 52.397 dB), the
new scores remain approximately **.989/1.913 dB lower**. Those references are
not contemporaneous paired competitors, so this is a target comparison, not a
freshly measured head-to-head gap. The every-axis goal remains unmet.

### Why screen-relay differs

Inspected the **installed, unmodified 0.1.2** engine source and tag
[`v0.1.2`](https://github.com/vercel-labs/screen-relay/tree/0a0a0a9c7e3ddee0b035e3ecd659e86fa7d57d21).
Installed `src/engine/core/capture.cpp` hashes identically to that tag
(`1196bc7d85fec0bbba52fdda1ceb14ab6c6367607d2a256a9ea3482be22e0f5d`).
The installed engine hashes identically to the benchmark runtime snapshot
(`87487a836556e11efaa17b97556eb1447954bf30f22aa3f5d00521da01aad140`).
The last campaign's actual engine log reports **Constrained Baseline, level
3.2, 4:2:0, 8-bit**. This rules out a 4:4:4/high-profile explanation.

`Resources::openEncoder` in `src/engine/core/capture.cpp` unconditionally selects
**libx264**, preset **ultrafast**, tune **zerolatency**, **CRF 10**, **NV12
(4:2:0)**, no B frames, no lookahead and no periodic IDRs. Scene-cut IDRs are
disabled; session/recovery forces IDR. It signals limited-range BT.709, unlike
nanoctl's correctly signaled limited BT.601 conversion. Changed macroblocks are
passed as `mb_info`; unchanged blocks can avoid redundant work. Its 150-ms idle
refinement path is not an explanation for these continuously moving workloads.

CRF 10 is a quality target, **not equivalent to fixed QP10 or minimum QP10**.
Motion uses VBV max rate from the adaptive flow controller, with four frame
budgets of buffer; keyframes/refinement use eight. CLI defaults start at 8 Mbps
with a 1–40-Mbps range. It is not unconstrained CRF, but spends more bandwidth:
the earlier screen-relay text/drag payloads were 3.362/7.528 Mbps, versus QP14's
2.249/3.854 Mbps here. CRF policy, encoder decisions and different conversion
rounding are not individually ablated; no measured percentage of the remaining
quality difference is assigned to one setting.

### Cheap lower-floor probes: not retained

Two additional immutable builds changed only the minimum QP to 10 or 6, leaving
maximum 51 and adaptive control intact. Each got **one fresh paired text run and
one fresh paired drag run** against QP14, reversing order between workloads.
All eight runs were valid (960/960 inputs). These are pilots, **not five-pair
gate validations**, and neither lower floor is retained:

| Prototype | Workload | PSNR Y dB |  Mbps | Input p50 Δ ms | Video p50 Δ ms | CPU Δ points |
| --------- | -------- | --------: | ----: | -------------: | -------------: | -----------: |
| QP10      | Text     |    52.906 | 2.527 |         +3.677 |          −.039 |       −1.118 |
| QP10      | Drag     |    51.361 | 4.697 |         −4.010 |          −.239 |       −1.691 |
| QP6       | Text     |    53.134 | 2.503 |         +3.642 |          +.163 |       +2.880 |
| QP6       | Drag     |    51.332 | 4.603 |         +2.714 |          +.097 |       +5.487 |

QP10 buys +1.543/+.915 dB over its paired QP14 text/drag controls for
+.277/+.840 Mbps; QP6 buys +1.740/+.577 dB for +.263/+.789 Mbps. Text exceeds
the earlier screen-relay PSNR reference, **but drag still trails by about
1.04/1.06 dB**. These probes do not establish a gap-closing drag configuration.
QP10's text input delta and QP6's input/drag CPU deltas exceed the limits in
their single pairs. QP6 also loses .83/.73 FPS; shared-VM scheduling and content
sampling are not isolated, so these are observations, not proof of an intrinsic
QP-induced latency penalty or quality ceiling. No further floor was tried.

The measured gap-closing reference is screen-relay's **whole x264 CRF10 + VBV
pipeline**, not 4:4:4 or a higher H.264 profile. Mirroring that quality policy
would require an x264 backend or a more detailed OpenH264 rate-control,
deblocking and conversion ablation; matching only the numeric QP is insufficient.
The reference spends about **49% more text and 95% more drag payload bandwidth**
than validated QP14 here (3.362/7.528 versus 2.249/3.854 Mbps), already close to
the 8-Mbps drag gate. Native x264 CPU, latency, library weight and exact quality
are **unmeasured**, not assumed equal to the full screen-relay product. A new
backend also needs cross-platform packaging and x264 GPL/commercial-licensing
review. That is not a cheap prototype, so none was added; no guaranteed
gap-closing nanoctl configuration is claimed. **QP14 is the only retained,
fully validated change from this follow-up.**

Artifacts: `/home/ubuntu/bench-results/gradient-fidelity/`, including diagnostic
`gradient-trace-v2-control-*`, rejected `gradient-{software,vsync}-trace-*`,
`gradient-paired-*`, retained `fidelity-paired-*`, all inventories, source/client
logs, vmstat and matched images; `fidelity-prototype-*`/`fidelity-prototype6-*`
and `prototype{,6}-summary.json` preserve both lower-floor pilots.
`attribution-summary.json` and
`final-summary.json` preserve stage distributions, run vectors, validity and
paired/arm-median deltas. Drivers and analysis live under
`/home/ubuntu/bench-candidates/gradient-*`. These remain 720p software H.264,
loopback, captured-pixel and matched-ROI luma measurements, not optical latency,
tailnet capacity, chroma quality or matched-bitrate quality comparisons.
Native-pixel QP14/QP16 text client crops for `fidelity-paired-*-text-run-3` were
inspected; no optical or impaired-link check was performed.

# Previous head-to-head: QP16, desynchronized source, url 2.5.9

2026-11-23 UTC, 01:38–02:18. **This single campaign supersedes every older
three-product comparison below, but predates the QP14 follow-up above.** This
campaign's release includes QP16–51, rounded
conversion, `CameraVideoRealTime`, unchanged congestion control, and the source
canvas `{ alpha: false, desynchronized: true }` for **all three products**.
The security-only lockfile update is commit `1a32505`: `url` 2.5.9 requires
`form_urlencoded` 1.2.3; no other dependencies changed. Its CI passed all ten
required checks before this campaign started. Measured release SHA256:
`40e4e6868f2c238fc224f7a7b07634ee71318e00899d3a992d61f40cbf403a36`.

## Protocol and completeness

Exactly **42 measured runs**, with no replacement runs or excluded outliers:
marker ×5/product, moving text/drag/gradient ×3/product. Within each repetition
the order is nanoctl, Sunshine/Moonlight, screen-relay. Fresh agents, clients,
browsers and X servers per run; all **42 launch inventories** passed the
expected-engine/client check, exactly two Xvfb servers, and zero pre-existing
Chrome. No builds, profiling, extra source observers or offline reports ran
during measurements. The runner stopped after Moonlight's first incomplete
run, then resumed without remeasuring either completed run.

**41/42 runs satisfy the full validity predicate.** Moonlight marker run 1
matched **119/120** inputs: source counter 104 never appeared as a separate
paint, although the source received all 120 inputs. Its detector was healthy
(247.30 Hz; source clock uncertainty ±0.31 ms), but that trial has no observable
finite response latency. This is not proof of a lost input packet. The cause
of the coalesced/unobservable paint was not localized. The run remains flagged
`valid: false` and **is included** in all five-run Moonlight headline medians;
no timeout was imputed as a finite latency and no substitute run was selected.
Moonlight's input percentiles therefore describe observed responses, with one
unobservable trial reported separately. Nanoctl and screen-relay each matched
600/600 marker inputs; Moonlight matched 599/600. All 27 quality runs are valid.

Values are medians of per-run metrics, not pooled percentiles or confidence
intervals. Scope: 1280×720 software H.264, Chrome 151.0.7922.34, one shared Linux
VM, development-auth **loopback**, not a tailnet/impaired link. Video means
source draw → captured client pixels; input means injection → captured client
pixels, **not optical input-to-photon**. The approximately 250-Hz detector
quantizes observations. CPU uses one core = 100%; CPU/RSS cover host process
trees, excluding clients, browsers, Xvfb and detector. Whole-run VM steal was
0.70% mean, 39% maximum; these are shared-VM samples, not a portability claim.

## Marker latency, resources and runtime: five runs per product

| Product            | Inputs  | Input p50 / p95 ms | Video p50 / p95 ms |   FPS | CPU % | RSS MiB | Host runtime MiB |
| ------------------ | ------- | -----------------: | -----------------: | ----: | ----: | ------: | ---------------: |
| nanoctl QP16       | 600/600 |      31.81 / 40.07 |      19.80 / 22.15 | 59.92 | 25.43 |   44.80 |            23.47 |
| Sunshine/Moonlight | 599/600 |      32.30 / 40.80 |      22.56 / 28.63 | 59.86 | 43.44 |  165.86 |           124.98 |
| screen-relay       | 600/600 |      56.37 / 88.35 |      54.26 / 73.90 | 59.81 | 24.21 |   48.22 |           139.59 |

Runtime is logical host application/assets plus transitive ELF libraries,
excluding clients, browsers, Xvfb, toolchains, Docker layers and other OS files;
it is **not total install footprint**. Nanoctl's snapshot was rebuilt for this
binary; existing competitor snapshots were checked against their live files.

## Moving quality: three runs per product/workload

| Workload | Product      |  SSIM Y | PSNR Y dB |   Mbps |   FPS | Video p50 / p95 ms | CPU % | RSS MiB |
| -------- | ------------ | ------: | --------: | -----: | ----: | -----------------: | ----: | ------: |
| Text     | nanoctl QP16 | .999145 |    49.955 |  2.140 | 59.84 |      19.50 / 21.52 | 29.45 |   44.70 |
| Text     | Moonlight    | .999343 |    48.415 |  5.309 | 59.86 |      17.10 / 21.24 | 53.92 |  155.73 |
| Text     | screen-relay | .999577 |    52.202 |  3.362 | 59.58 |      24.58 / 27.43 | 29.04 |   48.45 |
| Drag     | nanoctl QP16 | .998226 |    49.596 |  3.306 | 59.95 |      19.63 / 21.90 | 32.37 |   48.18 |
| Drag     | Moonlight    | .999163 |    48.572 |  7.759 | 59.89 |      24.95 / 28.68 | 49.17 |  156.75 |
| Drag     | screen-relay | .999564 |    52.397 |  7.528 | 59.78 |      24.31 / 26.69 | 29.99 |   48.46 |
| Gradient | nanoctl QP16 | .994576 |    47.931 |  7.007 | 59.58 |      35.90 / 38.02 | 48.78 |   49.78 |
| Gradient | Moonlight    | .992738 |    45.905 |  7.266 | 59.84 |      31.96 / 36.45 | 58.65 |  154.64 |
| Gradient | screen-relay | .994068 |    47.670 | 16.563 | 59.81 |      51.99 / 58.36 | 42.13 |   48.25 |

Quality is matched-content ROI, gray luma, moving rather than settled text.
Bitrate scopes differ: nanoctl video-RTP payload; screen-relay decoded
WebSocket payload including protocol/control; Sunshine video-port UDP payload
including framing/FEC. These are configured-product results, **not
matched-bitrate quality or identical wire-traffic measurements**. No new
impaired-link or hardware-codec results are claimed.

## Remaining deficits: every losing measured median

Differences below use unrounded medians. Tiny gaps are reported for completeness,
not asserted statistically significant; many are smaller than detector polling
quantization. Percentage-point CPU differences use one core = 100%.

- **Marker:** nanoctl uses **1.216 CPU points more than screen-relay**.
  Its other marker headline medians beat both products, but its 0.493-ms input
  p50 advantage over Moonlight is below one detector poll.
- **Text versus Moonlight:** video p50 **+2.402 ms**, p95 **+.279 ms**;
  FPS **−.0147**; SSIM **−.000198**. Versus screen-relay: CPU **+.412 points**,
  PSNR **−2.247 dB**, SSIM **−.000432**.
- **Drag versus Moonlight:** SSIM **−.000937**. Versus screen-relay:
  CPU **+2.385 points**, PSNR **−2.800 dB**, SSIM **−.001338**;
  p99 frame interval **+.080 ms** (20.491 versus 20.412 ms).
- **Gradient versus Moonlight:** video p50 **+3.945 ms**, p95 **+1.567 ms**,
  FPS **−.2546**. Versus screen-relay: CPU **+6.645 points**,
  RSS **+1.531 MiB**, FPS **−.2239**.

There are no other losing medians among input/video p50/p95, FPS, CPU, RSS,
runtime, measured payload Mbps, SSIM, PSNR, p95/p99 frame intervals and >100-ms
stall counts where those metrics apply. Stall-count medians are zero for all
products/workloads. Total marker stalls were nanoctl/Moonlight/screen-relay
**1/2/1**; total gradient stalls **0/0/7**; text and drag had none. Runtime is
smaller than both competitors, but complete installation size was not measured.

**The every-axis goal remains unmet.** Nanoctl gradient video p50s were
**19.878, 36.067, 35.903 ms** despite the fixed source options. This campaign
did not instrument their segments. The follow-up above localizes the dominant
delay before capture, but no reliable mitigation was retained. Do not claim
that all latency modes are eliminated.

Artifacts: `/home/ubuntu/bench-results/final-qp16/final-qp16-*`, including all
42 JSONs, inventories, detector/source logs, vmstat and matched quality images.
`summary.json` includes medians, min–max ranges, validity, input and stall
counts. Reproduction/analysis drivers:
`/home/ubuntu/bench-candidates/final-qp16-{campaign,stack}.ts` and
`final-qp16-summary.py`. Native-pixel text source and all three client crops for
`*-text-run-2` were inspected. All product stacks and the screen-relay container
are stopped.

# Supporting evidence: source-presentation investigation and QP16 validation

2026-11-23 UTC. **The refresh-sized mode is upstream of nanoctl capture, in the
benchmark source's canvas-to-Xvfb presentation.** The harness now uses an opaque,
desynchronized source canvas. This is not a viewer, pacer or encoder latency
optimization. Fresh unprofiled paired validation passed, and QP16 now passes the
unchanged text/drag gates. Production retains rounded conversion and
`CameraVideoRealTime`, lowering only the minimum QP to **16**; maximum QP remains
**51**, with adaptive congestion control unchanged.

## Fast versus slow: per-frame attribution

November 20 diagnostic runs `mode-source-nanoctl-marker-run-2` (fast) and `-6`
(slow), with identical QP20 binaries, source observers and browser attribution.
Marker IDs join source draw timestamps, independent source-Xvfb pixel reads,
agent monotonic logs, browser video metadata and client pixel observations.
There are 3,489/3,435 joined browser/agent samples respectively. Values below are
per-segment p50 milliseconds, not additive components of a pooled median.

| Segment                                            |   Fast |   Slow |
| -------------------------------------------------- | -----: | -----: |
| Source draw → independently observed source pixels |  3.342 | 19.040 |
| Source draw → damage dequeued                      |  1.510 | 17.567 |
| Damage dequeued → capture start                    |  0.036 |  0.037 |
| Capture start → end                                |  1.532 |  1.624 |
| Capture end → encode start                         |  0.278 |  0.290 |
| Encode start → end                                 |  2.360 |  2.409 |
| Encode end → RTP sample write completed            |  0.164 |  0.167 |
| RTP sample write completed → Chrome receiveTime    |  0.244 |  0.061 |
| Reported decode duration                           |  0.400 |  0.400 |
| Chrome receiveTime → presentationTime              |  0.700 |  0.700 |
| presentationTime → expectedDisplayTime             |  8.600 |  8.900 |
| presentationTime → detector observation            | 16.763 | 17.410 |
| expectedDisplayTime → detector observation         |  8.366 |  8.711 |
| Source draw → detector observation, run video p50  | 24.275 | 40.759 |

**Localization:** 16.06 ms of the 16.48-ms end-to-end difference is already
present at damage dequeue. Independent source-pixel observation also shifts by
15.70 ms, excluding a nanoctl damage-wait, encode, RTP or client-detector queue as
the origin of this mode. Receive-to-presentation and decode duration do not gain
a frame. The verified cause is refresh-sized **source presentation delay**,
not slow conversion or a nanoctl receiver queue. The exact internal Chromium
buffer/deadline responsible was not separately instrumented.

Damage logging timestamps event dequeue, not the X server's event creation.
RTP timing brackets the application sample write, not a hardware transmit clock.
Decode duration is Chrome metadata; adding it to receiveTime would only estimate
decoder completion. Clock offsets/uncertainties remain in each JSON. The 250-Hz
source observer can first see a pixel up to roughly one poll after it appears;
it can therefore observe a frame after nanoctl has captured it. Attribution
readback perturbs absolute latency: the profiled modes are about 24/41 ms,
versus about 19/35 ms in unprofiled runs. Use them for segment localization, not
headline product comparisons.

### Competitor controls and attempted disproof

The same source-side modes occur with both competitors. Below are **per-run
source-draw → source-pixel p50s**, not product transport latency:

| Stack              | Workload / repetitions | Source-pixel p50s, ms                      |
| ------------------ | ---------------------- | ------------------------------------------ |
| nanoctl            | Marker ×6              | 3.390, 3.342, 3.453, 3.386, 19.693, 19.040 |
| Sunshine/Moonlight | Marker ×5              | 18.656, 19.075, 3.484, 3.473, 3.266        |
| screen-relay       | Drag ×5                | 4.168, 4.000, 18.936, 4.065, 19.644        |

Their end-to-end p50s were respectively 24.10–41.55, 14.32–40.60 and
51.43–70.11 ms; these diagnostic cohorts have different instrumentation and are
**not** a new head-to-head. Source delay does not explain all competitor
transport/client variation. Screen-relay's source observer ran inside the
container's IPC namespace; initial host-side MIT-SHM attachment failed, and
those preflights are excluded. Six source-only legacy trials did **not**
reproduce the slow mode, so its reproduction without any product is unverified.

An attempted `--run-all-compositor-stages-before-draw` source fix failed: one
run's source-pixel/video p50 reached 35.06/56.44 ms. It is not retained.
Disabling threaded compositing failed the source's visibility/readiness check
and is also not retained. The kept combination is
`getContext("2d", { alpha: false, desynchronized: true })`; which individual
option is necessary was not isolated. Chrome accepted both attributes. Six
instrumented fresh nanoctl stacks then had source-pixel p50s **3.195–3.321 ms**
and video p50s **24.174–24.495 ms**, with no slow mode observed. Six source-only
low-latency trials were also fast, but the legacy source-only trials were fast
too. This is a measured fix on this browser/Xvfb setup, not a portable guarantee
about the desynchronized hint.

## Strict paired harness validation: five fresh runs per arm/workload

November 23, 00:11–00:37 UTC; **20/20 valid**, each matching 120/120 inputs.
Same immutable QP20 binary in both arms. Alternate legacy/fixed order within
pairs; fresh agents/browsers/X servers, only the expected product stack, audited
inventories, and no tracing, extra source observers, builds or profiling.
Marker uses ordinary timing; drag combines quality and input timing. Values are
medians of per-run metrics; CPU is one core = 100%.

| Workload | Source | Input p50 / p95 ms | Video p50 / p95 ms |   FPS | CPU % | RSS MiB |
| -------- | ------ | -----------------: | -----------------: | ----: | ----: | ------: |
| Marker   | Legacy |    32.055 / 36.494 |    19.947 / 22.128 | 59.95 | 25.44 |   42.05 |
| Marker   | Fixed  |    32.063 / 36.473 |    19.862 / 21.951 | 59.95 | 25.24 |   44.32 |
| Drag     | Legacy |    32.213 / 52.371 |    20.123 / 37.486 | 59.81 | 31.32 |   48.79 |
| Drag     | Fixed  |    32.073 / 48.388 |    19.059 / 34.842 | 59.90 | 30.71 |   49.58 |

Per-run video p50s (pair order; no headline outliers removed):

| Workload | Legacy, ms                             | Fixed, ms                              |
| -------- | -------------------------------------- | -------------------------------------- |
| Marker   | 19.993, 32.764, 19.843, 19.947, 19.642 | 20.102, 19.805, 19.862, 19.863, 19.744 |
| Drag     | 20.123, 19.765, 50.755, 35.446, 18.745 | 19.059, 18.851, 18.917, 19.279, 19.061 |

No high video-p50 mode was observed in the ten fixed runs. This does not mean
tails or individual inputs are deterministic: fixed drag video p95 remains
34.842 ms. The legacy 50.755-ms outlier was not separately instrumented, so its
entire excess is not independently localized. Drag quality/bitrate was
46.922→47.038 dB, SSIM .997019→.997146, 1.947→1.954 Mbps; source rendering
options changed, so do not interpret that as an encoder quality improvement.

The pre-interruption cohort `mode-paired-*` is not mixed into this table. It has
nine valid marker runs plus one invalid run with 24/120 inputs and a
**232,717,124-ms detector gap** across the quota interruption. All are preserved.
After resume, HEAD and origin both matched `8bb1514`; managed Chrome was stopped,
screen-relay was stopped, and no active benchmark stacks or uprobes remained.
The whole paired cohort was restarted as `mode-paired-v2-*` after startup load
cleared.

## QP16 re-test: retained, all unchanged aggregate gates pass

November 23, 00:38–01:06 UTC; **20/20 valid**, five fresh alternating pairs each
on text and drag, 120/120 inputs/run. Both arms use the fixed source and combined
quality/input trials. These are independent runs from the harness validation.

| Workload | QP floor |  SSIM Y | PSNR Y dB |  Mbps | Input p50 / p95 ms | Video p50 / p95 ms |   FPS | CPU % | RSS MiB |
| -------- | -------: | ------: | --------: | ----: | -----------------: | -----------------: | ----: | ----: | ------: |
| Text     |       20 | .998260 |    46.560 | 1.617 |    28.324 / 36.350 |    18.565 / 20.716 | 59.95 | 27.52 |   47.68 |
| Text     |       16 | .999150 |    50.027 | 2.159 |    28.352 / 36.339 |    18.571 / 20.690 | 59.96 | 29.49 |   48.75 |
| Drag     |       20 | .997137 |    46.903 | 1.957 |    28.398 / 36.496 |    18.900 / 22.216 | 59.97 | 29.85 |   50.35 |
| Drag     |       16 | .996948 |    49.297 | 3.322 |    28.460 / 36.392 |    18.799 / 20.981 | 59.93 | 32.69 |   51.97 |

Differences of arm medians, as in the previous sweep:

| Workload | Input p50 Δ ms | Video p50 Δ ms | CPU Δ points | CPU ≤51% / Δ≤5 | Latency Δ≤2 ms | Drag ≤8 Mbps |
| -------- | -------------: | -------------: | -----------: | -------------- | -------------- | ------------ |
| Text     |          +.028 |          +.006 |       +1.971 | Pass           | Pass           | n/a          |
| Drag     |          +.061 |          −.101 |       +2.840 | Pass           | Pass           | Pass         |

Median paired deltas also pass: text input/video/CPU −.024/+.041/+1.978;
drag +.061/−.363/+2.666. Individual input-p50 pair deltas remain variable:
text **+.175, −.066, −.167, +3.515, −.024 ms**; drag
**+.596, +.061, +3.548, −3.867, +.036 ms**. No individual-pair ≤2-ms claim is
made. The gate is the same arm-median criterion used in the earlier sweep, not
a newly relaxed threshold. The passing re-test does not prove the earlier
QP16 drag input-only rejection was caused by source presentation delay.

QP16 gains **3.467 dB text / 2.394 dB drag PSNR**, but drag median SSIM falls
slightly. QP14 was not tested in this requested QP16 retry. The production
effective-parameter regression now asserts QP16–51. Rebuilt release SHA256
`a64d50ea3c19dfa85f967a0e9d3c1c248b7af859f8e6b29c1fb695b5f0a6bfa8`
is byte-identical to the measured immutable QP16 candidate. No congestion,
bitrate-controller, complexity, usage, threading or pacing code changed.

## Scope, artifacts and remaining limits

720p software H.264, Chrome **151.0.7922.34**, one shared Linux VM,
development-auth **loopback**, not a measured tailnet/impaired link. Video is
source draw to captured client pixels; input is injection to captured client
pixels, **not optical input-to-photon**. A 250-Hz detector still quantizes
observations. CPU/RSS are host-process-tree only, excluding source/client
browsers, Xvfb and detector. Quality is matched-ROI gray luma, moving content,
not matched-bitrate or optical quality. The opaque source may change rendering;
historical quality/latency tables are not a new contemporaneous comparison.

The earlier three-product table below is historical, not evidence that the new
QP16 build beats either competitor. No new three-product final campaign was
requested in this handoff. Runtime/install weight was not remeasured; the
every-axis goal remains unproven. Impaired links, hardware codecs, physical
displays and the precise internal Chromium presentation deadline were not tested.

Artifacts: `/home/ubuntu/bench-results/latency-modes/`, prefixes
`mode-trace-*`, `mode-source-*`, `mode-sync-*` (rejected), `source-lab-*`,
`mode-desync-*`, `mode-paired-v2-*`, and `mode-qp-*`.
`latency-pairs-summary.json` preserves all per-run metrics/ranges; external
drivers and analysis are in `/home/ubuntu/bench-candidates/latency-*.{ts,py}`.
Native-pixel text source/QP16/QP20 crops for `mode-qp-*-text-run-3` were inspected.
All benchmark stacks are stopped; the screen-relay container is stopped.

# Previous isolated comparison: QP20 quantizer sweep (legacy source)

2026-11-20 UTC. **No lower QP floor retained.** QP18 and QP16 improved text
quality within the CPU and bitrate budgets, but failed drag latency gates.
QP14 was not attempted because QP16 did not pass. Production remains exactly
`5ce62e5`: rounded integer conversion, `CameraVideoRealTime`, QP20–51, unchanged
adaptive congestion control. The final rebuilt executable is byte-identical to
the immutable QP20 control (SHA256
`a40044d2b46af5424aca3a21aa32ffebebec88d4dc049edbefe2c6f0c42db210`).

## Previous three-product campaign

**One fresh campaign, 05:26–06:05 UTC: 42/42 valid runs.** Marker ×5/product,
plus moving text/drag/gradient ×3/product. Products alternate within each
repetition: nanoctl, Sunshine/Moonlight, screen-relay. Every run starts a fresh
stack, audits exactly two Xvfb servers and only the expected engine/client,
and rejects pre-existing Chrome, agents, encoders and frame readers. All 42
inventories passed. No builds, profiling or offline reports overlap measurements.
Postflight: no live benchmark stacks; screen-relay container stopped.

Scope: one shared Linux VM, 1280×720, software H.264, development-auth loopback,
not an actual tailnet or impaired link. Input is **injection to captured client
pixels**, not optical input-to-photon. Video is source canvas draw to captured
client pixels. The approximately 250-Hz detector contributes up to roughly 4 ms
polling quantization. CPU is one core = 100%; CPU/RSS include the host process
tree, not clients, source browsers, Xvfb or the detector. Values are medians of
per-run percentiles/averages, not pooled distributions or confidence intervals.

### Marker: five repetitions per product

All products matched **600/600 inputs**. Runtime is host application/assets plus
transitive ELF libraries, not build directories, clients/browsers, Docker layers
or a complete cross-platform installation footprint.

| Product            | Input p50 / p95 ms | Video p50 / p95 ms |   FPS | Motion CPU % | RSS MiB | Host runtime MiB |
| ------------------ | -----------------: | -----------------: | ----: | -----------: | ------: | ---------------: |
| nanoctl QP20       |      32.16 / 40.22 |      20.14 / 22.40 | 59.96 |        24.65 |   43.92 |            23.47 |
| Sunshine/Moonlight |      29.00 / 40.04 |      18.82 / 22.27 | 59.86 |        43.58 |  166.36 |           124.98 |
| screen-relay       |      60.06 / 72.07 |      53.76 / 60.36 | 59.73 |        25.14 |   48.36 |           139.59 |

Min–max across the five runs (not percentile confidence bounds):

| Product      | Input p50 ms | Input p95 ms | Video p50 ms | Video p95 ms |         FPS |       CPU % |       RSS MiB |
| ------------ | -----------: | -----------: | -----------: | -----------: | ----------: | ----------: | ------------: |
| nanoctl      |  31.92–48.09 |  36.38–67.15 |  19.82–37.11 |  21.90–46.63 | 59.28–60.00 | 24.25–36.82 |   41.71–44.83 |
| Moonlight    |  24.18–32.10 |  32.33–40.38 |  14.56–20.92 |  18.37–26.61 | 59.57–59.96 | 42.96–46.76 | 164.04–169.21 |
| screen-relay |  36.17–80.08 |  52.33–88.35 |  27.65–71.72 |  55.18–87.88 | 59.32–59.93 | 23.64–27.33 |   48.19–48.66 |

### Moving quality: three repetitions per workload and product

SSIM/PSNR are luma-only, matched source/client frames in the content ROI;
quality capture follows the 20-second timing phase. These are moving, not settled,
text scores. Every quality run has at least ten matched pairs. Browser/native
focus clicks and pointer parking are preserved. Nanoctl source/client text crops
from repetition two were inspected at native pixels.

| Workload | Product      |  SSIM Y | PSNR Y dB |   Mbps |   FPS | Video p50 ms | Motion CPU % |
| -------- | ------------ | ------: | --------: | -----: | ----: | -----------: | -----------: |
| Text     | nanoctl      | .998301 |    46.655 |  1.612 | 59.80 |        19.38 |        28.03 |
| Text     | Moonlight    | .999342 |    48.415 |  5.318 | 59.90 |        27.17 |        59.64 |
| Text     | screen-relay | .999577 |    52.214 |  3.364 | 59.75 |        44.28 |        30.64 |
| Drag     | nanoctl      | .997812 |    47.146 |  1.948 | 59.94 |        19.28 |        28.84 |
| Drag     | Moonlight    | .999160 |    48.567 |  7.756 | 59.74 |        18.70 |        52.80 |
| Drag     | screen-relay | .999557 |    52.398 |  7.527 | 59.70 |        24.18 |        30.13 |
| Gradient | nanoctl      | .993764 |    46.862 |  5.037 | 59.21 |        35.53 |        46.35 |
| Gradient | Moonlight    | .992597 |    45.492 |  7.280 | 60.00 |        32.54 |        65.17 |
| Gradient | screen-relay | .994044 |    47.684 | 16.568 | 59.68 |        25.19 |        39.38 |

Bitrate scopes differ: nanoctl inbound video-RTP payload, screen-relay decoded
WebSocket payload, Sunshine video-port UDP payload including framing/FEC. This
is a defaults comparison, **not matched-bitrate quality**. Actual tailnet
capacity, optical latency, hardware encoding, chroma quality and installation
weight on other platforms are not measured.

The every-axis goal is **not met**: nanoctl still trails text/drag fidelity,
Moonlight marker p50 latency, and screen-relay gradient CPU/latency. Isolation
does not remove VM scheduling variability: whole-run `vmstat` (excluding each
first since-boot sample) reports mean steal **0.61%, maximum 32%**. No headline
outliers were removed or replaced. Historical headline tables below are
superseded by this contemporaneous campaign, not mixed into it.

## QP floor sweep: strict paired text and drag

Each candidate is compared to the immutable `5ce62e5` QP20 build, three fresh
pairs per workload, CT/TC/CT order. **24/24 runs valid**, each matching 120/120
inputs; all 24 isolation inventories passed. `BENCH_QUALITY_INPUT=1` extends
motion to 59 seconds and measures input latency before matched quality capture.
Both members use the same phase plan and parent-surface video fullscreen.
The final campaign instead uses ordinary marker and 20-second quality phases;
do not substitute its faster controls or different scene poses into these pairs.

Gates were fixed before analysis: workload-median CPU ≤51% and ≤paired control
+5 points; input/video p50 ≤control +2 ms; drag bitrate ≤8 Mbps. Gates compare
medians of three per-run metrics; all pair-level deltas remain in raw artifacts.
There is no new subjective quality threshold. The effective max QP stays **51**
for every candidate; no congestion-controller, bitrate or feedback changes.
Existing effective-parameter and 4000→250-kbps congestion regressions remain.

| Sweep | Workload | Min QP     |  SSIM Y | PSNR Y dB |  Mbps | Input p50 ms | Video p50 ms |  CPU % |
| ----- | -------- | ---------- | ------: | --------: | ----: | -----------: | -----------: | -----: |
| 18    | Text     | 20 control | .998260 |    46.528 | 1.617 |       44.170 |       35.015 | 28.057 |
| 18    | Text     | 18         | .998716 |    47.961 | 1.786 |       28.217 |       18.625 | 27.653 |
| 18    | Drag     | 20 control | .997019 |    46.921 | 1.956 |       28.405 |       18.690 | 29.345 |
| 18    | Drag     | 18         | .996764 |    48.109 | 2.436 |       40.357 |       34.783 | 30.984 |
| 16    | Text     | 20 control | .998277 |    46.583 | 1.610 |       28.176 |       18.592 | 28.399 |
| 16    | Text     | 16         | .999091 |    50.027 | 2.149 |       28.185 |       18.800 | 29.082 |
| 16    | Drag     | 20 control | .996749 |    46.922 | 1.959 |       28.605 |       18.669 | 29.109 |
| 16    | Drag     | 16         | .996948 |    49.297 | 3.322 |       32.042 |       18.935 | 30.731 |

| Candidate | Workload | Δ CPU points | Δ input p50 ms | Δ video p50 ms | Decision                             |
| --------- | -------- | -----------: | -------------: | -------------: | ------------------------------------ |
| QP18      | Text     |       −0.404 |        −15.953 |        −16.390 | Pass workload                        |
| QP18      | Drag     |       +1.639 |        +11.952 |        +16.092 | Reject candidate: both latency gates |
| QP16      | Text     |       +0.683 |         +0.010 |         +0.207 | Pass workload                        |
| QP16      | Drag     |       +1.621 |         +3.437 |         +0.266 | Reject candidate: input gate         |

QP16's drag input deltas by pair are **+0.004 / +3.661 / +3.437 ms**: the median
paired delta also exceeds +2 ms. CPU and bitrate pass for both candidates, but
the input limit was not loosened to one detector poll. Refresh-sized variability
also exists in unchanged controls; these measurements do not isolate the exact
cause of the latency differences. **Keep QP20; QP14 not attempted**, as conditioned
on QP16 passing. The quality gap remains despite the promising 50.027-dB text
result at QP16.

One initial combined-mode preflight (`qp-sweep-18-control-text-run-1`) was invalid:
92/120 matched inputs and stalled client presentation with direct video-element
fullscreen. Its source registered all 120 inputs. It is preserved, excluded from
both completed cohorts, and not reclassified as an encoder result. After changing
the combined-mode fullscreen surface to the video parent, all 24 complete sweep
runs matched 120/120 and approximately 60 FPS. Ordinary final-campaign phase
behavior is unchanged.

Artifacts: `~/bench-results/qp-floor/qp-sweep-v2-*`, `qp-final-*`,
`sweep-{18,16}-summary.json`, `final-head-summary.json`; each run retains raw
JSON/pixel/source traces, process inventory, logs, `vmstat`, quality pairs and
crops (plus Sunshine PCAP/counters). Binaries/build logs and ordered drivers:
`~/bench-candidates/{quality-gap-qpsweep-*,quality-gap-qp-final,qp-floor-*}`.
Native crop evidence: `qp-final-nanoctl-text-run-2.quality-text-{source,client}.png`.
Runtime snapshots: `~/bench-runtime/{qp-floor-final,sunshine-final-quality,screen-relay}`.

## Prior retained change: rounded conversion and encoder-path diagnosis

2026-11-20 UTC. **Retained:** nearest-code unsigned integer RGB→I420 conversion,
with OpenH264's general-purpose `CameraVideoRealTime` usage instead of
`ScreenContentRealTime`. This is still screen capture, software H.264, QP20–51,
and the existing adaptive congestion controller. It is not a camera capture mode.
The supported usage setting bypasses OpenH264's expensive screen-only feature
motion estimation (FME) heuristic; no private ABI or dependency fork is used.
Complexity, frame skipping, thread configuration, bitrate allocation, VUI and
pacing settings are unchanged. Idle refinement is not part of this change.

## Motion-phase profiling

Control and rounded binaries have identical release/debug flags and diagnostics;
only conversion arithmetic differs. `perf record -g`, 199 Hz, DWARF stack dumps,
attaches to the agent PID during drag motion, not the browser or whole system.
The requested event is `cycles:u`; the recorded data identifies `task-clock:uH`.
These are recorded-event **self percentages, not hardware-cycle measurements or
inclusive call-graph weights**. The reason for the event remapping was not isolated.
Offline report processing and builds are excluded from the final paired trials.
The table uses `round-profile-control-drag-run-3` and the first completed rounded
and camera profiles. Earlier control artifacts are exploratory: the first had a
sudo-child shutdown error; the second overlapped a stalled offline DWARF report.
The clean control repeat, ablation, entry-counter runs and retention trials have
no such background report. Only self reports are used, not stalled inclusive
DWARF reports.

| Self symbol                                                           | Screen/truncation | Screen/rounded | General-purpose/rounded |
| --------------------------------------------------------------------- | ----------------: | -------------: | ----------------------: |
| `CaptureEncoder::encode_captured_frame` (includes inlined conversion) |            21.41% |         16.83% |                  26.81% |
| `WIDTH_LOOP_SSE4`                                                     |             1.53% |         13.07% |               0 samples |
| `HASH_WIDTH_LOOP_SSE2`                                                |             1.46% |         11.77% |               0 samples |
| `WelsVP::CheckLine`                                                   |            16.27% |         12.01% |               0 samples |
| capture-thread Rust entry                                             |             9.29% |          7.36% |                  11.37% |
| `WelsSampleSad16x16_sse2`                                             |             2.61% |          2.00% |                   1.08% |
| `WelsSampleSad8x8_sse21.pixel_sad_8x8_nsplit`                         |             3.07% |          1.82% |                   2.89% |

Per-stage wall counters below use the first and last one-second snapshots fully
inside motion: 1,199 / 1,200 / 1,200 attempts over approximately 20 seconds. Native
is backend minus conversion; it includes output copying and diagnostic overhead.
Profile runs are diagnostic observations, not the retention gates.

| Metric                               | Screen/truncation | Screen/rounded | General-purpose/rounded |
| ------------------------------------ | ----------------: | -------------: | ----------------------: |
| Capture ms/attempt                   |            1.6171 |         1.6409 |                  1.6249 |
| Convert ms/attempt                   |            1.1948 |         1.1667 |                  1.2916 |
| Backend ms/attempt                   |            4.5280 |         6.2928 |                  3.7363 |
| Native ms/attempt                    |            3.3332 |         5.1261 |                  2.4447 |
| Encode attempts/s                    |           59.9485 |        60.0015 |                 59.9994 |
| Empty encodes / native skipped delta |             0 / 0 |          0 / 0 |                   0 / 0 |
| Pacer skips / IDR delta              |            1 / 10 |         2 / 10 |                  2 / 10 |
| Motion CPU %                         |            37.638 |         47.497 |                  32.954 |
| Video p50 ms                         |            19.466 |         36.319 |                  19.361 |

The original rounded kernel is effectively equal in this comparison; native
encoding adds **1.7929 ms/frame**, approximately 10.76 CPU points at 60 FPS if
CPU-bound, consistent with the observed +9.86 points. One additional encode and
one additional pacer skip over 20 seconds cannot account for that CPU increase;
native skipped and IDR counts are identical. The higher conversion wall time in the
general-purpose diagnostic run is included, not discarded; retention uses the
separate unprofiled trials below.

### Verified costly path and causal ablation

Bundled OpenH264 (`openh264-sys2` 0.9.7), in
`encoder/core/src/encoder_ext.cpp::InitDqLayers`, allocates feature
storage for screen-content usage. On screen-content P frames,
`PreprocessSliceCoding` calls `CalcFMESwitchFlag`, then `PerformFMEPreprocess`
when enabled and the reference features have not yet been calculated.
`svc_motion_estimate.cpp::CalculateFeatureOfBlock` dispatches to the sum and
feature-location routines containing the two hot assembly loops above.

This path is observed, not inferred only from source: fresh motion-only entry
uprobes on `WelsEnc::PerformFMEPreprocess`, counted with `perf stat -p`, recorded
**78 calls / 20.2767 s for truncation versus 867 / 20.2873 s for rounding**,
an 11.1× increase. Both binaries use the same native configuration and dimensions;
only the YUV values differ. Thus the additional CPU is data-dependent **screen
feature-search preprocessing**, not the integer conversion or extra frame count.

The switch depends on detected scrolling or a positive FME-good-frame history
with average macroblock SAD >30. Which operand/particular pixel values trigger
the difference was **not instrumented**; an exact white-level explanation is not
claimed. Source dispatch plus runtime entry counts verify the costly path.
Low complexity does not directly gate this preprocessing block. Rather than
blindly changing several encoder settings, the supported general-purpose usage
is a causal ablation: the screen-only branch is unreachable, its three hot
symbols disappear, native time falls and the CPU/latency penalty disappears.
This usage also changes other screen-specific coding decisions; it is not
claimed to be a one-heuristic-only switch. Drag PSNR in the ablation is about
0.5 dB below screen-mode rounding, but still over 4 dB above shipping truncation.

The near-equal pacer-skip counts do not establish a missed 8.3-ms pacing gate.
The refresh-sized video delay could be a downstream presentation deadline;
that exact latency mechanism remains unverified. No optical latency claim is made.

## Strict unprofiled retention gates: drag and text, three pairs each

Twelve fresh-agent runs, all valid, 1280×720, balanced software encoding and
loopback; ordering is control/treatment, treatment/control, control/treatment for
each workload. Control is the retained QP20/truncating executable. Treatment is
rounded conversion plus the general-purpose usage setting. Encoder debugging,
perf and builds are absent. Each inventory enforces zero pre-existing Chrome,
the expected engine/client only, and two Xvfb instances. Medians are of per-run
metrics, not pooled samples; CPU is one core =100%. Bitrate is inbound video RTP.

| Workload | Variant  |  SSIM Y | PSNR Y dB |  Mbps |   FPS | Video p50 ms | Motion CPU % | RSS MiB |
| -------- | -------- | ------: | --------: | ----: | ----: | -----------: | -----------: | ------: |
| Drag     | Control  | .997237 |    42.813 | 1.892 | 60.00 |       35.937 |       34.692 |  56.890 |
| Drag     | Retained | .997812 |    47.146 | 1.950 | 59.99 |       19.279 |       28.323 |  46.018 |
| Text     | Control  | .997150 |    41.725 | 1.102 | 59.78 |       19.401 |       44.735 |  52.906 |
| Text     | Retained | .998244 |    46.592 | 1.609 | 59.90 |       19.393 |       27.123 |  42.916 |

Drag: **+4.333 dB, −6.369 CPU points, −16.659-ms video p50**.
Text: **+4.867 dB, −17.612 CPU points, −0.008-ms video p50**.
The last latency delta is indistinguishable from measurement noise, not an
attributable improvement. Both workloads pass ≤approximately +3 CPU points and
no median video-latency regression. Individual text-pair latency deltas are
+0.054 / +1.307 / −0.026 ms; one treatment run has a 21.390-ms video p50.
All three drag pairs improve video p50 by 16.59–16.79 ms. No outlier is excluded.

Per-run ranges: drag control/treatment CPU 34.396–36.193 / 27.995–29.140%,
video p50 35.863–36.184 / 19.275–19.398 ms; text CPU 44.638–45.841 /
26.735–33.412%, video p50 19.339–20.083 / 19.375–21.390 ms. Text bitrate rises
approximately 46%, versus 3% for drag: this is not a matched-bitrate comparison.
The adaptive controller still owns congestion; the existing real-encoder
4000→250-kbps regression passes with the new usage setting. Effective QP20–51,
bitrate rate control, general-purpose usage and color metadata are checked for
all three latency modes. Other modes' performance, impaired links and hardware
encoders were not benchmarked in this round.

This closes the measured rounding CPU problem, **not the goal of beating both
competitors on every axis**. The earlier competitor campaign below is retained
as a baseline, not relabeled as a new contemporaneous comparison with this fix.

Artifacts: `~/bench-results/clean-final/round-profile-*`, `round-count-*`, and
`round-fixed-*` (raw JSON, inventories, logs, perf data, counter files, matching
frames and crops). Native-pixel control/treatment text crops were inspected:
`round-fixed-{control,fixed}-text-run-2.quality-text-client.png`. No new UI or
input-after-idle trial was requested or performed.

# Previous isolated-stack comparison (before the rounding fix)

Measured consecutively on 2026-11-20 UTC, 02:12–02:52, at 1280x720 on Xvfb,
software H.264 and loopback, with default product settings otherwise. Each
measurement had only its own streaming host/client stack: other engines,
viewers, browsers and displays were stopped. Pre-run inventories enforce zero
pre-existing live Chrome processes, exactly the expected streaming engine/client
and two Xvfb servers before launching the benchmark browsers.

This is input-to-captured-client-pixels, **not optical input-to-photon**. CPU is
one core = 100%; host CPU/RSS excludes browsers, Xvfb, readers and native clients.
The marker and moving-content rows are different workloads, not interchangeable
resource numbers. Cells are medians of per-run metrics; brackets are min–max,
not confidence intervals or pooled percentiles. The comparison below measures
the retained QP20–51/truncating-conversion baseline, before the idle experiment.

## Marker: five valid runs per product

| Product            | Input p50 ms        | Input p95 ms          | Video p50 ms        | FPS                 | Motion CPU %        | Motion RSS MiB         | Host runtime MiB |
| ------------------ | ------------------- | --------------------- | ------------------- | ------------------- | ------------------- | ---------------------- | ---------------- |
| nanoctl QP20       | 36.07 [32.17–36.39] | 52.30 [40.32–76.81]   | 34.58 [20.15–52.44] | 59.86 [58.45–59.88] | 39.77 [38.14–53.31] | 54.74 [53.18–58.54]    | 23.47            |
| Sunshine/Moonlight | 35.98 [28.04–64.06] | 48.19 [43.77–84.56]   | 24.81 [14.41–48.60] | 53.93 [52.22–59.93] | 51.41 [48.41–77.50] | 167.24 [167.01–171.96] | 124.98           |
| screen-relay       | 80.33 [56.05–96.05] | 104.06 [68.18–124.02] | 86.64 [50.83–88.49] | 59.75 [59.71–59.94] | 27.41 [24.94–28.41] | 48.15 [47.98–48.34]    | 139.59           |

All products matched **600/600 inputs; 15/15 marker runs were valid**. No run was
excluded or replaced. Nanoctl and Moonlight input p50 differ by less than the
approximately 4-ms detector quantization; this does not establish an input-latency
winner between them. Moonlight's lower observed unique FPS is included, not
filtered out. Idle CPU was 0.78 / 48.86 / 0.58%, respectively.

Runtime is logical host-side installed bytes including transitive ELF libraries:
nanoctl release binary; Sunshine binary and `/usr/share/sunshine`; screen-relay
npm package/engine, Node and listed helpers. Browsers, Xvfb, Moonlight client,
build tools/caches, Docker layers and unrelated OS files are excluded. The
baseline snapshots contain 24,605,552 / 131,051,595 / 146,374,952 bytes.

## Moving content: three valid runs per product/workload

| Workload | Product            | SSIM Y  | PSNR Y dB | Received Mbps | FPS   | Video p50 ms | Motion CPU % |
| -------- | ------------------ | ------- | --------- | ------------- | ----- | ------------ | ------------ |
| Text     | nanoctl QP20       | .997157 | 41.722    | 1.098         | 58.37 | 36.97        | 63.93        |
| Text     | Sunshine/Moonlight | .999342 | 48.416    | 5.278         | 58.83 | 21.92        | 63.63        |
| Text     | screen-relay       | .999577 | 52.209    | 3.361         | 59.92 | 40.55        | 28.18        |
| Drag     | nanoctl QP20       | .997237 | 42.847    | 1.891         | 59.53 | 36.48        | 43.18        |
| Drag     | Sunshine/Moonlight | .999161 | 48.569    | 7.750         | 59.84 | 25.32        | 61.00        |
| Drag     | screen-relay       | .999562 | 52.379    | 7.527         | 59.85 | 52.56        | 31.35        |
| Gradient | nanoctl QP20       | .993468 | 44.815    | 5.128         | 59.05 | 36.70        | 54.63        |
| Gradient | Sunshine/Moonlight | .992758 | 46.046    | 7.301         | 59.82 | 43.19        | 71.56        |
| Gradient | screen-relay       | .994075 | 47.688    | 16.562        | 59.71 | 68.69        | 47.77        |

**27/27 quality runs were valid.** Bitrate scopes differ: nanoctl inbound-video
RTP payload; screen-relay decoded WebSocket payload including protocol messages;
Sunshine video-port UDP payload including framing/FEC. These are not equivalent
wire rates or matched-bitrate quality measurements. Full-frame quality capture
and FFmpeg processing followed the separate CPU/latency measurement.

### Isolation evidence and historical limitations

The requested initial `top -b -n1` and `ps aux --sort=-%cpu | head -30` found old
Sunshine PID 21124, Moonlight 21958, screen-relay engine 137681, Chrome and Xvfb
processes. They were stopped; Moonlight required SIGKILL after SIGTERM. The
snapshot showed no zombies and approximately 100% idle CPU, so resident
processes alone do not prove they were actively encoding or caused slow controls.

A later audit caught **ten persistent managed-browser Chrome processes** on
display `:0` that the first cleanup missed. PID 3426 used `.browser_data_dir` and
remote-debugging port 29229. At this audit its targets were only the new-tab UI;
the root/GPU processes reported lifetime CPU of approximately 0.3/0.2%. This is
not evidence of a competing video stream, nor proof of its state during earlier
runs. Nevertheless it violated strict browser isolation. It was stopped with
SIGTERM, all Chrome/streaming/Xvfb processes were verified gone, and **the entire
42-run headline cohort plus six rounding runs were repeated with the stronger
zero-pre-existing-Chrome check**. No platform browser tools were used afterward.

Earlier `clean-*`, `clean-parked-*`, `clean-round-*`, `refine-*` and partial
`exact-*` artifacts are preserved but superseded for strict-isolation comparison.
The older nanoctl marker attempt with 119/120 acknowledgements is not part of the
headline cohort above. Older Moonlight quality captures with a client cursor
inside the ROI have a demonstrated quality artifact: on a same-pair crop,
excluding that cursor changed PSNR from approximately 39.76 to 48.17 dB. The
runner now keeps its necessary focus click, then parks both pointers outside
the scored ROI; the new headline uses that corrected setup throughout.

Isolation does **not** guarantee an uncontended physical host. VM CPU steal and
refresh-sized latency modes remain, with large run-to-run ranges. Sunshine's
20:53:18 November 19 disconnect preceded the QP20 probe at 22:06 and rounding
controls at 23:23; active Sunshine streaming is not established as their cause.
Historical rows are marked **non-isolated/superseded**, not proven to have their
entire CPU/latency deficit caused by contamination. The isolated nanoctl text CPU
is still 63.93%, not consistently restored to the earlier approximately 37–50%.

## Strict paired rounding recheck: rejected

Three fresh-agent control/treatment drag pairs (CT, TC, CT), **six valid runs**,
after the headline cohort. The same unsigned fixed-point candidate and prior
conversion microbench apply: 1,364,967 versus 1,408,994 ns/frame, **+3.23%**, inside
the approximately 10% conversion gate. The production patch replaces arithmetic
inside the existing conversion, not an additional conversion call. The isolated
kernel does not explain the whole-agent CPU delta.

| Variant          | SSIM Y  | PSNR Y dB | Mbps  | FPS   | Video p50 ms | Motion CPU % |
| ---------------- | ------- | --------- | ----- | ----- | ------------ | ------------ |
| QP20 control     | .997262 | 42.770    | 1.896 | 59.69 | 19.89        | 40.65        |
| Unsigned rounded | .998690 | 47.705    | 1.896 | 58.87 | 37.50        | 51.54        |

Quality improves, but **CPU +10.89 points** and observed video p50 **+17.60 ms**
exceed the approximately five-point/two-ms gates. Rounding is not retained.
The experiment does not identify the cause of the whole-pipeline CPU increase.

## Exact normal-context idle refinement

Built and tested the requested one-shot **150-ms, QP14 P-frame** policy on the
normal software OpenH264 context, with **no bitrate/clean-feedback admission
condition**. It cancels only immediately before native encoding if newly captured
damage is waiting; once encoding starts it finishes, then handles new damage.
There is no second encoder, and cancellation never aborts a started native encode.

The reached native non-reset update path does not apply global QP bounds alone.
The prototype temporarily sets the layer QP, disables periodic intra for that
call, selects `RC_OFF_MODE`, and restores parameters and rate-control function
pointers afterward. Tests cover P/non-IDR output, decoder continuity through the
next normal frame, cancellation/empty input, restoration on an injected encoding
error, and a subsequent 4000→250-kbps allocation cut. Hardware encoders and an
impaired-link refinement campaign were not tested.

Three fresh-agent pairs (CT, TC, CT) for input and each moving workload: **24/24
valid runs**. Both input variants matched **360/360 inputs**. The treatment logged
**363 QP14 frames** across the three input sessions, including warmup, rather
than skipping pauses through the earlier exploratory admission condition.
Observed stable intervals before Space were at least 502.75 / 646.08 ms for
control/treatment. The settled phase captures 13 deterministic scenes per run
after at least 500 ms unchanged: **39/39 paired source-scene PPM hashes matched**.

| Variant         | Input-after-idle p50 ms | p95 ms              | Settled SSIM Y            | Settled PSNR Y dB      |
| --------------- | ----------------------- | ------------------- | ------------------------- | ---------------------- |
| QP20 control    | 32.11 [28.35–40.74]     | 48.02 [40.12–82.47] | .997164 [.997017–.997236] | 41.729 [41.681–41.777] |
| QP14 refinement | 32.27 [28.24–32.28]     | 44.31 [36.35–52.08] | .997653 [.997636–.997703] | 42.526 [42.524–42.528] |

**The numeric input gate passes: p50 +0.17 ms, below the user's <=3-ms limit**
and the approximately 4-ms detector quantization. The p95 difference is not
claimed as a causal improvement. Settled quality improves reproducibly by
**0.797 dB**, with non-overlapping PSNR/SSIM ranges on identical source scenes.

**Not retained under the predeclared 1-dB interpretation of a "clear" settled
quality gain.** That was an engineering judgment, **not a numerical threshold
specified by the user**. The gain is real but small in the inspected native-pixel
text crops, unlike the approximately five-dB rounded-conversion improvement.
This is a quality-gain retention decision, not a claim that the exact policy
failed its numeric input gate or moving-content guardrails.

### Moving-content guardrails

| Workload | Variant | SSIM Y  | PSNR Y dB | Mbps  | FPS   | Video p50 ms | Motion CPU % |
| -------- | ------- | ------- | --------- | ----- | ----- | ------------ | ------------ |
| Text     | Control | .997150 | 41.731    | 1.102 | 59.74 | 19.49        | 44.74        |
| Text     | QP14    | .997150 | 41.725    | 1.102 | 59.69 | 19.37        | 46.19        |
| Drag     | Control | .997205 | 42.828    | 1.892 | 59.85 | 36.21        | 37.97        |
| Drag     | QP14    | .997262 | 42.770    | 1.892 | 59.66 | 19.57        | 37.66        |
| Gradient | Control | .993457 | 44.822    | 5.183 | 59.48 | 35.60        | 47.96        |
| Gradient | QP14    | .993474 | 44.818    | 5.149 | 59.43 | 21.66        | 47.41        |

All three motion CPU deltas are within five points; none of the median video
latencies rises by two ms. Refresh-sized latency modes and differing ranges mean
the apparent reductions are not attributable performance wins. Even the unchanged
strictly isolated text controls varied: headline CPU/video p50 63.93%/36.97 ms
versus the later paired control's 44.74%/19.49 ms. Both used the same executable
with no unrelated browser/stream. Guest cleanup did not eliminate VM/scheduling
variation, so the later fast control is not substituted into the headline table.

### Encode blocking and bitrate burst

Across all 363 logged input-session refinements, including warmup, encode time
including conversion was **p50 4.819, p95 11.782, max 49.559 ms**. One encode is
the bound, **not a hard five-ms bound** on this shared VM. Payload was **p50
2,079 B, p95 3,995 B, max 55,767 B**. The >=500-ms input trial does not deliberately
collide with the 150-ms encode and is not a worst-case in-flight blocking test.

Mean receiver video-RTP rate in approximate 100–400-ms post-paint windows was
**0.0221→0.0754 Mbps**, and 400–700-ms quiet windows were **0→0 Mbps**. Whole
idle-input-trial receive rate was **0.0417→0.0568 Mbps**. The median per-run maximum
refinement-window bin was **4.70→4.03 Mbps**; these approximately 100-ms bins can
include delayed interactive/IDR traffic, so they are not isolated refinement
wire peaks. Encoded frame bytes above directly measure the one-frame burst.
The existing two-second keepalive remains separate; there is no repeated idle
quality loop.

The earlier admitted QP14 prototype (`refine-*`) had an extra >=4-Mbps/clean-link
condition and the unrelated managed browser still present. It is exploratory,
not the exact requested policy or a strict-isolation retention result. The partial
`exact-*` trial was stopped before this complete `strict-exact-*` campaign.

The shipping software path remains **QP20–51, truncating conversion and the
ordinary 250-ms refresh**. The unconditional prototype's executable and source
patch are preserved as `~/bench-candidates/idle-refinement-always14{,.patch}`;
no prototype Rust changes remain in the retained source.

Artifacts: `~/bench-results/clean-final/strict-*`, summaries
`strict-summary-corrected.json` and `strict-exact-idle-summary.json`, per-run
`.inventory.log` / `.vmstat.log`, `strict-round-*` and `strict-exact-*`. Inspected
native-pixel crops:

- `strict-moonlight-text-run-2.quality-text-client.png`
- `strict-exact-control-quality-text-run-2.settled-text-client.png`
- `strict-exact-always14-quality-text-run-2.settled-text-client.png`
- `strict-exact-always14-quality-text-run-2.settled-text-source.png`

## Historical campaigns (superseded; original values retained)

## screen-relay Docker versus current nanoctl, five runs each

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

The explicit maximum is 51 (the effective sentinel default was 35), so
allocation cuts can sacrifice fidelity/frame output;
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

## Last quality round, part 1: isolated conversion and unsigned rounding

No floats or data-dependent per-pixel branches were present in the rejected
four-addition rounding implementation. A new ignored release microbench isolates
conversion from capture/encoding, with reusable 720p buffers and nine rotated
50-frame batches. The original kernel is checked against the packed-RGB library
reference; the rounded candidate is checked byte-for-byte against the earlier
signed rounded transform over black, white and pseudorandom frames and several
even dimensions. Production remains the original truncating kernel.

| Kernel                   | First microbench ns/frame median [min–max] | Final production-control microbench ns/frame median [min–max] |
| ------------------------ | ------------------------------------------ | ------------------------------------------------------------- |
| Current truncation       | 1449076 [1272133–2226845]                  | 1364967 [1279732–2451640]                                     |
| Earlier signed rounding  | 1398815 [1281993–1924919]                  | 1379793 [1298477–2056445]                                     |
| Unsigned 16-bit rounding | 1574649 [1276694–1767070]                  | 1408994 [1299948–2128354]                                     |

The candidate uses bounded unsigned 16-bit sums, folds offsets into 4224/32896,
and explicitly averages chroma with `(sum + 2) >> 2`. Maximal intermediates are
60324 for Y and 61456 for U/V, so neither wrapping nor saturation is needed.
Actual release assembly mapped to the conversion source contains 52 packed-word
SIMD operations (`pmullw`, `paddw`, `psllw`, `psrlw`, `psubw`); scalar cleanup
remains. Assembly evidence: `~/bench-candidates/final-quality-conversion.asm.txt`.
The first microbench is +8.67% versus control; the final benchmark calls the
production `read_rgba` method directly for control and measures +3.23%.
Both meet the approximately 10% microbench gate. Crucially, the earlier signed
rounding also does **not** reproduce a 13-point conversion cost in isolation.
This disproves simply blaming floating-point math or scalar conversion overhead;
it does not establish the cause of the whole-agent CPU differences.

The actual unsigned candidate was still evaluated with three fresh-agent pairs
per workload, order control/treatment, treatment/control, control/treatment.
All 18 runs were valid. No builds or competing streams ran during measurement.
Values are medians of per-run metrics; spreads remain in
`~/bench-results/content/final-quality-paired-*.repeat.json`.

| Workload | Variant          | SSIM Y  | PSNR Y dB | Mbps | FPS   | Video p50 ms | Motion CPU % |
| -------- | ---------------- | ------- | --------- | ---- | ----- | ------------ | ------------ |
| Text     | QP-20 control    | 0.99719 | 41.71     | 1.10 | 58.42 | 53.21        | 66.71        |
| Text     | Unsigned rounded | 0.99884 | 46.93     | 1.09 | 57.84 | 37.66        | 68.53        |
| Drag     | QP-20 control    | 0.99728 | 42.90     | 1.89 | 58.31 | 37.49        | 53.99        |
| Drag     | Unsigned rounded | 0.99870 | 47.66     | 1.87 | 57.15 | 37.72        | 63.46        |
| Gradient | QP-20 control    | 0.99346 | 44.81     | 5.15 | 57.17 | 53.08        | 80.70        |
| Gradient | Unsigned rounded | 0.99393 | 47.02     | 5.11 | 56.63 | 54.18        | 78.43        |

**Rejected:** drag CPU +9.47 points exceeds the approximately five-point gate,
despite the microbench and median latency gates passing. All control/treatment
results are retained. Absolute control CPU/latency also deteriorated versus
earlier sessions; these are not new competitor comparisons. A subsequent VM
sample showed no cgroup throttling and 100% idle in its one-second sample; that
does not establish conditions during the runs. Cumulative native timing logs
cover moving-quality capture as well as the motion phase and do not isolate a
cause for the motion-only CPU difference. No claim of a confirmed codec,
compiler or host-scheduling root cause is made.

The candidate executable/patch and raw logs are preserved under
`~/bench-candidates/final-quality-rounded-u16`, `final-quality-u16.patch` and
`final-quality-*`. Only the ignored microbench, reference checks and documentation
are retained; unsigned rounding is **not enabled** in the production encoder.
