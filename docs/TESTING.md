# Verification strategy

## Automated repository gates

CI runs formatting, oxc lint, TypeScript 7 project checking, Bun unit tests, transactional Convex
authorization tests, Rust format/clippy/tests, the production OpenNext/Sites build, dependency
audits, and secret scanning. Native media builds run on Linux, macOS, and Windows. Local
`bun run check` is the fast pre-commit subset; it does not replace the browser, production-build,
media-feature, or security jobs.

The `release candidates` workflow compiles the full media agent on native x64 and arm64 runners for
Linux, macOS, and Windows. It creates timestamp-normalized archives, SHA-256 checksums, a CycloneDX
SBOM, and GitHub build-provenance attestations. These artifacts are deliberately called
**candidates**: they are unsigned and are not an installable release.

Current automated coverage includes bounded protocol and signaling parsing, role/session identity,
duplicate enrollment and signaling, owner isolation, revocation, terminal mutation idempotency,
rate-limit windows, TURN configuration, update signature/digest/rollback behavior, media buffer and
bitstream transforms, input bounds and fail-safe release behavior, packaging transactions,
production configuration, CSP nonces, media-evidence validation, and an in-process connection
between the production host peer and a real WebRTC controller peer through SDP, ICE, DTLS/SCTP,
and control-channel opening.

## Required release-test expansion

The following remain mandatory before a supported release; they are requirements, not claims about
the current automated suite:

- property/fuzz tests for security-sensitive JSON/control parsers and state transitions;
- replay, duplicate, gap, expiry, wrong-role, wrong-owner, revoked-token, and cross-device tests;
- authorization tests around every public Convex function and HTTP action;
- malformed SDP/candidate tests and data-channel flood/backpressure tests;
- log snapshots proving secrets, clipboard, SDP, and ICE candidates are redacted.

## Browser tests

The checked-in Chromium suite covers the unauthenticated gate, Shoo origin/redirect binding,
malformed callback containment, response CSP/nonces, authenticated dashboard loading/empty/device
states, readiness affordances, activity history, pairing, rename, removal, sign-out, and operation
failure containment. The authenticated fixtures reuse the production dashboard view through an
explicit test-server gate; production preflight rejects that gate. The same suite drives the
production viewer controller through a deterministic WebRTC browser peer and verifies display
commands, button and keyboard emergency release, page/unmount cleanup, terminal state, and bounded
reconnect exhaustion. Full browser-to-native media interoperability, Firefox and WebKit, plus real
Chrome and Edge performance remain release gates because synthetic peers and media are insufficient
for decoder performance.

## Native tests

Native unit tests cover configuration, update transactions, media conversion/queue behavior,
hardware-fallback policy, input parsing/bounds, signaling identity, and peer-failure grace. A
real two-peer localhost test covers production host signaling, SDP/ICE negotiation, connection, and
control-channel opening, then performs and reconnects through an ICE restart. Expansion of that
harness to synthetic 4K motion, impairment, TURN-only, permission loss, sleep/wake, network switch,
and abrupt controller death is still required.

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

Run the native capture/encoder gate from the signed candidate on every physical host:

```shell
nanoctl --config ./acceptance.toml media-smoke --require-hardware --seconds 1800 --json > media-smoke.json
```

The command opens the real platform capture session, requires the hardware backend, encodes at the
configured dimensions/FPS/bitrate, rejects malformed or non-Annex-B output, and requires observed
IDR, SPS, and PPS units. It exits 2 when it produces fewer than 75% of the requested frames or any
required bitstream evidence is absent. Use `--seconds 3600` for the longest single invocation and
repeat under the network/controller soak for the eight-hour gate. Preserve the JSON together with
external process RSS/GPU/thermal traces; this command does not substitute for input-to-photon,
browser decode, memory-growth, or network measurements.

Validate the six 30-minute hardware records as one release set:

```shell
bun run evidence:media -- evidence/*/media-smoke.json
```

The verifier rejects missing or duplicate OS/architecture targets, mixed agent versions, software
fallback, short runs, failed records, and absent bitstream counters. A passing set is necessary but
does not replace the Linux X11 software-fallback record or the remaining manual and network gates.

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
