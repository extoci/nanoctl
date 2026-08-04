# Next version goal: nanoctl smooth desktop

The next release should feel like a native remote-desktop product, not a collection of
components: a Windows host is ready after login without a visible console, a Mac Chromium client
can end and immediately reopen a session, and the first usable frame arrives quickly while motion
stays smooth and current.

This is an optimistic but achievable release target for the current architecture. It keeps the
per-user interactive Windows task required by desktop capture, keeps the existing WebRTC protocol
compatible, and concentrates engineering effort on lifecycle ownership, bounded media handoff, and
health-gated packaging.

## Ship criteria

### Windows host experience

- A fresh install, reinstall, reboot, and upgrade leave one hidden, limited, interactive
  `nanoctl Agent` task for the logged-in user.
- Startup creates no visible console window. Runtime diagnostics go to the ACL-protected
  `agent.log`; the CLI remains available for explicit operator commands.
- Reinstalling over the prior Program Files layout migrates the existing explicit configuration and
  enrollment instead of silently asking the user to enroll again.
- Updates are serialized, stage-verified, startup-health-gated, and rollback-safe. The previous
  executable is retained until the candidate survives a short startup stability window; a failed
  transaction can be retried without manually deleting update artifacts.
- The task action always carries the exact configuration path, so a path/layout migration cannot
  accidentally start the agent against a new empty configuration.

### Session reliability

- Every session owns its signaling sequence, peer, input channels, capture worker, and encoder.
- Teardown is idempotent and cooperative. The blocking capture/Media Foundation worker is joined
  before the host accepts a replacement session.
- A browser viewer that is retained across route/session changes resets sequence and processed-signal
  state, ignores callbacks from an older generation, and sends sequence-zero for each new offer.
- The regression path “Windows host + Mac Chromium client: connect → end → connect again” succeeds
  for 20 consecutive cycles in a release smoke run, including one cycle after an ICE restart.
- Rejected, cross-session, stale-generation, and terminal signals cannot mutate the replacement
  peer.

### Smoothness and latency

- Capture remains newest-frame-first at every handoff. No unbounded encoded queue or catch-up burst
  is permitted.
- Recovery requests (PLI/FIR) result in a queued keyframe that is not discarded merely because the
  consumer is briefly busy.
- H.264 advertises loss recovery feedback (NACK/PLI/FIR/REMB), uses low-delay pacing, and forces a
  clean keyframe after display or encoder reconfiguration.
- Release evidence records first-frame time, capture-to-encode delay, encode p50/p95, frame drops,
  packet loss, RTT, decoded FPS, and rendered resolution at 1080p60 and 4K60 where hardware
  supports it.
- On a healthy LAN, the target is first frame under 500 ms after the host answer and p95 capture to
  encoded handoff under one frame interval. Under constrained bandwidth, quality may reduce, but
  interactive input must remain responsive and latency must not grow without bound.

## Delivery sequence

1. Land lifecycle and signaling regression tests first; keep the close/reopen path red until both
   browser and native cleanup are covered.
2. Ship cooperative media teardown and bounded newest-frame handoff, then run the native Windows
   hardware matrix.
3. Ship the hidden task action, config-layout migration, update lock, and health/rollback checks;
   validate fresh install, public-to-public upgrade, and legacy Program Files migration on clean
   Windows users.
4. Run the browser smoke matrix on Chromium and Safari/WebKit clients, then collect five-minute
   LAN and relay evidence before tagging the release.

## Explicit non-goals for this release

- A machine-wide Windows service: desktop capture and per-user credentials require the interactive
  user task.
- A tray UI: the background agent should be invisible and quiet first; a tray surface can be added
  after the lifecycle is proven.
- New audio, clipboard, VP9, or AV1 protocol features.
- Unbounded buffering or a quality preset that hides network congestion by accumulating latency.

The release is ready only when the ship criteria are demonstrated, not merely when the installer or
the WebRTC peer compiles.
