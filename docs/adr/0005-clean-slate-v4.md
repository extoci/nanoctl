# ADR 0005: Clean-slate nanoctl 4 architecture

## Status

Proposed. Keeps ADR 0004's tailnet-native trust and deployment model; replaces the 3.x session,
media, input, viewer, and regression-test implementation rather than patching them further.

## Context

`v3.0.0-alpha.6` was the last broadly usable build. The alpha.6-to-alpha.19 delta added more than
11,000 lines, largely as point fixes and performance experiments layered onto four already-large
modules (`media.rs`, `rtc.rs`, `remote-session.ts`, and `adaptive.rs`). The result is not one
coherent design: lifecycle ownership, loss recovery, backpressure, input delivery, encoder control,
and viewer rendering are distributed across several files and repaired locally whenever a symptom
appears. That makes every new optimization capable of silently regressing another axis.

The measured evidence is worth keeping. On the same Windows VM, Sunshine+Moonlight reached a
19.4 ms mean glass-to-glass proxy while nanoctl's best working path reached about 29.5 ms. Data
channel video plus WebCodecs is therefore not the problem; uncontrolled implementation complexity is.
Browser constraints still rule out Moonlight's raw UDP/ENet protocol, but its architectural ideas are
directly relevant: independent control/input/video/audio lanes, explicit loss recovery, bounded queues,
latest-state input, low-delay encoders, and deliberate frame pacing.

This is a clean-room rewrite. No Moonlight or Sunshine code is copied. Concepts are reimplemented
from public behavior, protocol documentation, web standards, and nanoctl's own measurements.

## Decision

Build nanoctl 4 as a small set of single-owner state machines with typed boundaries. A feature is not
done when it works once; it is done when its invariants are enforced by deterministic tests and its
release metrics meet the gates below.

### Architectural planes

```text
Tailnet HTTPS control plane
  auth, host info, display/capability discovery, signaling, updates

Session plane
  one finite-state session owner; generation-safe signaling; idempotent teardown

Host media plane
  capture -> damage/normalize -> encode -> lane writer
  every handoff bounded, timestamped, newest-frame-only where staleness is harmful

Client media plane
  reassemble -> validate reference chain -> decode -> pace -> present
  every queue bounded and observable

Input plane
  browser/native events -> compact wire snapshots/events -> host input state -> OS injection
  lossy latest-state traffic never delays reliable discrete traffic

Audio plane
  host loopback capture -> Opus -> realtime playback
  independent of video failure and input permission

Observability plane
  monotonic timestamps, counters, structured events, health snapshot, regression evidence
```

### Non-negotiable invariants

1. **One owner per lifecycle.** A session generation owns its peer connection, signaling task,
   capture, encoder, lane writers, input state, and timers. Cross-generation messages are ignored by
   construction. Replacement and terminal failure use the same teardown path and join blocking workers.
2. **No unbounded queues.** Every queue has a capacity and a written drop policy. Video capture,
   encode input, decode input, and presentation use newest-frame-wins. Control events are reliable and
   ordered. Pointer/gamepad analog state is latest-state and may drop stale values.
3. **No catch-up bursts.** Missed capture/render deadlines are skipped. Timestamps come from a
   monotonic capture clock and are never replaced with send or completion time.
4. **Explicit recovery.** A missing video reference invalidates dependent deltas exactly once, sends a
   rate-limited keyframe request through reliable control, and resumes only on a complete valid IDR.
   Periodic IDRs are not a recovery mechanism.
5. **Input safety is host-authoritative.** Disconnect, blur, permission loss, lane failure, or a liveness
   timeout releases every key/button/pad. Host-side repeat generation is deterministic; network jitter
   must not create stuck or repeated keys.
6. **Backpressure is visible.** Buffered bytes, queue depth, dropped frames, encode/decode time,
   loss, reordering, RTT, keyframe requests, and presentation delay are first-class counters.
7. **No hidden mode matrix.** Wire capabilities and latency/quality modes are negotiated explicitly.
   Unsupported combinations fail negotiation rather than degrading into implementation-defined behavior.
8. **Tailnet-native remains unchanged.** No hosted control plane, STUN, TURN, enrollment, accounts,
   or device credentials. Tailscale remains identity, reachability, and underlying encryption.

### Transport shape

WebRTC remains the browser transport because it is the only practical browser API for encrypted UDP
plus data channels. nanoctl 4 treats it as a substrate, not as the application architecture:

- signaling stays on the tailnet HTTPS WebSocket and carries protocol version, session id,
  controller/host epochs, ICE generation/ufrag, sequence, and monotonic send time;
- reliable ordered control carries session commands, discrete input, clipboard, capability changes,
  and recovery requests;
- unordered loss-tolerant motion carries pointer and gamepad latest-state snapshots;
- unordered loss-tolerant video carries delta access-unit chunks;
- reference-safe video carries complete IDR access units with redundancy/retry policy designed for
  SCTP behavior rather than relying on periodic keyframes;
- audio uses the browser realtime audio path unless measurements show a data-channel implementation
  is better without regressing compatibility;
- RTP video remains a compatibility/fallback path only when explicitly negotiated, never a duplicate
  hidden copy of the data-channel stream.

Raw access units use one canonical wire specification with golden fixtures shared by Rust, TypeScript,
and Swift. Parsers must be total functions over byte slices: malformed input is rejected or ignored,
never panics, never partially mutates state, and never trusts lengths before bounds checking.

### Module boundaries

New implementation code is organized by ownership rather than by accident of history:

- `nanoctl-core`: wire types, validation, session state machine, input state machine, metrics types;
  no OS, networking, browser, or codec dependencies.
- `nanoctl-host`: session orchestration, authentication integration, lifecycle supervision, health.
- `nanoctl-media`: capture/encode abstractions and platform backends; no signaling or viewer logic.
- `nanoctl-transport`: WebRTC/lane implementation; no codec or OS-input logic.
- `packages/protocol`: TypeScript wire validators and generated golden-test vectors.
- `packages/client-core`: browser-independent session, reassembly, recovery, input and pacing logic.
- `apps/viewer`: presentation and UI composition only.
- `apps/ios`: native client shell over the same protocol and state-machine semantics.

Existing 3.x code may be mined for measurements, platform facts, and test cases, but not moved wholesale.
A module may only be ported after its public contract and failure behavior are written down.

## Regression contract

The first implementation slice must create the measurement harness before feature parity work begins.
Every pull request that touches media, input, transport, lifecycle, packaging, or viewer startup must
state which gates it affects. A performance PR must include paired before/after numbers on the same
machine and scene. A correctness PR must include a deterministic failing test before the fix when practical.

### Fast merge gates

- Rust, TypeScript, Swift compile/typecheck/lint/test on supported platforms.
- Protocol parser fuzz/property tests and golden cross-language vectors pass.
- Session state-machine tests cover replacement, duplicate signals, stale generations, ICE restart,
  socket loss, terminal failure, and teardown race injection.
- Host/viewer loopback connect-end-connect succeeds for 20 consecutive cycles.
- Input-release injection proves no stuck keys/buttons after disconnect, blur, and liveness timeout.
- Queue invariant tests prove bounded memory under producer/consumer imbalance.

### Release gates

Release evidence is collected against the prior release and, until parity is established,
`v3.0.0-alpha.6` as the known-usable baseline. “Better in every measurable way” means no worse on any
primary metric and better on the targeted metric, with noise budgets recorded.

| Metric                          | Initial release gate                                                                                           |
| ------------------------------- | -------------------------------------------------------------------------------------------------------------- |
| Attach to first decoded frame   | <= 500 ms on healthy LAN; reattach target <= 100 ms                                                            |
| Glass-to-glass proxy at 1080p60 | mean <= 30 ms and p95 <= 50 ms on the reference Windows loopback scene                                         |
| Input-to-photon proxy           | no worse than alpha.6 baseline; target mean <= 40 ms                                                           |
| Presented fps                   | >= 58 fps for a 60 fps source on healthy LAN                                                                   |
| Reconnect churn                 | 100/100 attach-end-attach cycles without wedged session or growing memory                                      |
| Packet-loss recovery            | 5% loss/reorder fault injection: input remains responsive; video reference recovery <= 500 ms after loss stops |
| Static desktop resource use     | no unbounded growth over 30 min; agent CPU/RSS no worse than baseline                                          |
| Quality/bandwidth               | at equal PSNR/SSIM, bandwidth no worse; at equal bandwidth, quality no worse                                   |
| Audio/video independence        | disabling or failing audio must not stall video or input                                                       |
| Upgrade safety                  | failed candidate startup restores prior binary/config in clean Windows test                                    |

### Safety-net implementation rules

- Fault injection is part of the core harness: loss, reordering, duplication, delay, jitter, truncated
  packets, malformed messages, slow consumers, decoder failure, encoder failure, capture starvation,
  socket death, process kill, and upgrade failure.
- Benchmarks record machine, OS, GPU, encoder, display, browser, scene, commit, and raw samples.
  Summaries without raw evidence are not release evidence.
- Alerts are actionable invariants, not logs. Examples: encoded queue depth > 1 for 500 ms, decode
  queue growth for 250 ms, keyframe requests > 2/s, input heartbeat timeout, or teardown exceeding 2 s.
- The default mode is the best user experience. Hidden environment-variable experiments are not
  shipping configuration and may not be required to make the default stable.

## Delivery sequence

1. Land this contract plus the cross-language wire/spec test scaffold.
2. Implement `nanoctl-core` protocol validation and deterministic session/input state machines.
3. Implement host capture/encode pipeline behind interfaces using synthetic captures first, then
   Windows DXGI and hardware/software encoders.
4. Implement transport lanes and browser client-core reassembly/recovery/pacing.
5. Implement the minimal viewer UI needed for the primary path: Windows host to iPhone Safari and
   desktop Chromium.
6. Port audio, clipboard, gamepad, display selection, host picker, iOS native shell, updates, and
   packaging only after the core loop passes the fast gates.
7. Keep 3.x available while v4 is experimental. When v4 reaches parity, switch the default in one
   small release PR and keep rollback possible.

## Consequences

- The rewrite is intentionally breaking. There is no requirement for v4 sessions to interoperate with
  3.x sessions, although installers must preserve configuration and permit rollback.
- Some existing micro-optimizations may initially be absent. They can return only with paired evidence
  and without violating the invariants.
- Browser video remains more complex than a native Moonlight client because raw UDP/RTP control is not
  available to the web platform. The design accepts that constraint but keeps Moonlight's discipline:
  explicit lanes, bounded queues, loss-aware recovery, input safety, and measured pacing.
- Features that cannot be made deterministic under these invariants are cut rather than carried forward
  as unstable complexity.
