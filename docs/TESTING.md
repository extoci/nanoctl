# Verification strategy

## Required gates

Every change runs formatting, oxc lint, TypeScript 7 project checking, Bun protocol tests, Rust
format/clippy/tests, the production OpenNext/Sites build, Convex function generation/type checking,
dependency audit, and secret scan.

The `release candidates` workflow compiles the full media agent on native x64 and arm64 runners for
Linux, macOS, and Windows. It creates timestamp-normalized archives, SHA-256 checksums, a CycloneDX
SBOM, and GitHub build-provenance attestations. These artifacts are deliberately called
**candidates**: they are unsigned and are not an installable release.

## Protocol and security tests

- property/fuzz tests for every JSON/control parser, size bound, coordinate, state transition, and
  configuration field;
- replay, duplicate, gap, expiry, wrong-role, wrong-owner, revoked-token, and cross-device tests;
- enrollment concurrency test proving one code creates at most one device;
- authorization tests around every public Convex function and HTTP action;
- malformed SDP/candidate tests and data-channel flood/backpressure tests;
- log snapshots proving secrets, clipboard, SDP, and ICE candidates are redacted.

## Browser tests

Playwright covers login callback state, unauthenticated routing, device empty/list/offline states,
pairing, session lifecycle, keyboard escape behavior, display switching, reconnect UI, and cleanup.
Chromium, Firefox, and WebKit are tested where their WebRTC codec support permits. Real Chrome and
Edge remain release targets because synthetic media is insufficient for decoder performance.

## Native tests

Unit tests use mock capture, encoder, input, credential store, clock, and control plane. Integration
tests run two peers with synthetic 4K motion, artificial loss/jitter/bandwidth, ICE restart, TURN
only, encoder crash, permission loss, sleep/wake, network switch, and abrupt controller death.

Physical-machine release matrix:

| Platform              | Architectures | Required paths                                    |
| --------------------- | ------------- | ------------------------------------------------- |
| Windows 11            | x64, arm64    | WGC, hardware H.264, service/user split, lock/UAC |
| macOS 14/15+          | x64, arm64    | ScreenCaptureKit, VideoToolbox, TCC grant/revoke  |
| Ubuntu/Fedora current | x64, arm64    | GNOME/KDE Wayland portals, PipeWire, VA-API       |
| Linux X11 reference   | x64           | XDamage/XTest, software fallback                  |

## Network matrix

Test same LAN, double NAT, symmetric NAT, IPv4-only, IPv6-only, CGNAT simulation, UDP blocked,
TCP-only TURN, TLS TURN on 443, 1–10% packet loss, 30–200 ms RTT, bandwidth step-down/up, interface
switch, five-second outage, and TURN loss. Capture connection setup time, selected candidate type,
RTT, loss, encode/decode time, dropped frames, QP, bitrate, and recovery time.

## Performance acceptance

On reference hardware, sustain 1080p60 for 30 minutes without monotonic memory growth, queue growth,
audio drift over 50 ms, or latency creep. Soak for eight hours at 1080p30. Measure idle resource use,
first-frame latency, input-to-photon using a high-speed camera, and quality under constrained
bandwidth. Averages cannot hide p95/p99 stalls.

## Manual release checklist

1. Build candidates from the exact reviewed tag and verify every provenance attestation, SBOM, and
   checksum.
2. Sign the native binary and installer with the platform publisher identity; notarize macOS
   artifacts. Rebuild the archive and attest the signed bytes.
3. Fresh install, enroll, reboot, reconnect, revoke, and uninstall on every target.
4. Verify publisher signatures/notarization and update rollback.
5. Verify no inbound listener and least-privilege file/credential ACLs.
6. Inspect logs and crash artifacts for sensitive content.
7. Run TURN-only from an unrelated external network.
8. Confirm lock/TCC/UAC/portal boundaries behave as documented.
9. Record exact hardware, OS, browser, agent, and test results in the signed release artifact.

Never publish or label an unsigned candidate as a release. Code-signing credentials stay outside
the repository and candidate workflow; the signing ceremony is a separate, access-controlled
release gate.
