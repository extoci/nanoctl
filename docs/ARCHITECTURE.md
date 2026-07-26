# Architecture

## Components

The browser is the controller. Next.js serves the application and static assets. Shoo authenticates
the human and issues an ES256 token scoped to the web origin. Convex verifies that token, stores
ownership and ephemeral coordination state, and exposes a narrow HTTPS agent API. The native agent
owns capture, encoding, WebRTC, and input injection. coturn is a packet relay of last resort.

```text
Shoo ── signed identity ──> Browser ── authenticated functions ──> Convex
                              │                                   ▲
                              │ DTLS-SRTP + SCTP                   │ HTTPS agent API
                              │ (direct or TURN)                   │
                              ▼                                   │
                         Native agent ───────── ICE ───────────────┘
                              │
                    OS capture / encoder / input
```

Convex is not in the media path. TURN cannot decrypt DTLS-SRTP. Signaling contains SDP and ICE
candidates, which reveal network addressing metadata and are therefore short-lived.

## Trust boundaries

1. The browser is untrusted until Shoo JWT verification succeeds in Convex.
2. The agent is untrusted until its bearer credential hash matches a non-revoked device.
3. Both peers validate every protocol message despite WebRTC transport integrity.
4. The native OS boundary decides whether capture and input are available.
5. TURN is an untrusted transport relay with time-limited credentials.

Device credentials authorize only agent endpoints for one device. User tokens authorize only data
owned by their Shoo `subject`. A session joins these identities for no more than fifteen minutes.

## Data model

- `devices`: owner, display name, platform, version/capabilities, last heartbeat, credential hash,
  and revocation status.
- `pairingCodes`: owner, SHA-256 code digest, creation/expiry, and one-time consumption marker.
- `sessions`: owner/device binding, strict state, expiry, and end reason.
- `signals`: session, role, monotonically increasing per-role sequence, serialized versioned
  envelope, and expiry.
- `auditEvents`: append-only security-relevant ownership and session actions.

Raw enrollment codes and agent bearer tokens are never stored. Signal rows and expired pairing rows
must be deleted by scheduled maintenance. Audit retention is a deployment policy.

## Session state machine

```text
requested ── controller offer ──> negotiating ── ICE/DTLS ──> connected
    │                                  │                         │
    └──────── timeout/end ─────────────┴──── failure/end ────────┘
                                      ▼
                                ended | failed
```

Only the owner can create or end a session. Only the bound device can publish host signals. Terminal
states cannot transition back. One active session per device is enforced transactionally.

## Media pipeline

The portable backend keeps one long-lived native capture session open. xcap currently selects WGC
on Windows and PipeWire on supported Wayland desktops, with platform fallbacks elsewhere. A
dedicated forwarding thread replaces a single pending frame atomically: if capture outpaces
encoding, obsolete frames are discarded rather than accumulated. RGBA frames are scaled and
converted to I420 for bounded OpenH264 software encoding, then emitted directly to the WebRTC RTP
packetizer. Decoder PLI/FIR feedback forces an IDR, with a two-second periodic IDR as a recovery
backstop. Display metadata is refreshed in the signed-in owner’s device capability view. A display
switch updates input origin/geometry and starts the replacement recorder before stopping the
existing recorder; the next encoded frame is an IDR.

The portable path consumes receiver-estimated maximum bitrate (REMB), clamps it between 250 kbps and
the local ceiling, and reconfigures OpenH264 only after a 20% decrease or 25% increase. Recreating
the encoder also produces a clean recovery point; the hysteresis avoids reinitialization churn.
OpenH264 runs in bitrate-control mode. Advanced latency policy maps responsiveness/balanced/quality
to low/medium/high encoder complexity and controls whether the encoder may skip frames.
On macOS, the same hysteresis recreates a hardware-confirmed VideoToolbox session, which also forces
a recovery frame. Its AVCC samples are length-checked and converted to Annex-B with copied SPS/PPS
before reaching the RTP packetizer. Direct capture-to-encoder GPU surfaces, Windows Media
Foundation, Linux VA-API, capture timestamps, and adaptive resolution/frame rate remain native
performance-path release gates, not properties claimed for the portable backend.

`quality.encoder` is a fail-closed policy rather than a hint. `auto` prefers a verified native
backend and may fall back to OpenH264, `software` forces the portable backend, and `hardware` fails
session startup if the platform backend is unavailable. This prevents an administrator who requires
GPU isolation/performance from silently receiving software encoding.

Input uses an unordered, zero-retransmit channel for pointer motion and a reliable ordered channel
for key/button transitions and lifecycle messages. Pointer motion is coalesced; key-up and button-up
are never dropped and are synthesized on disconnect. System audio and clipboard synchronization are
reserved for a later protocol capability and are never advertised by a v1 agent.

## Connectivity

Trickle ICE starts with host/server-reflexive candidates. Production configuration supplies TURN
over UDP, TCP, and TLS 443. TURN credentials are short-lived HMAC credentials minted for a session;
static public TURN passwords are forbidden. ICE restart handles address changes. If `relay` policy
is selected, no host candidates are exposed to the peer.

“Peer-to-peer” is an optimization, not a correctness assumption. Symmetric NAT, enterprise
firewalls, carrier networks, and UDP blocking make a relay necessary for dependable access.

## Why a native agent

The service is privileged and latency-sensitive. Rust provides small static-ish binaries, explicit
memory ownership, native APIs, and cross-compilation support. Bun remains the repository task runner
and the runtime for development tools; TypeScript 7 checks the web and shared protocol; oxc formats
and lints. The privileged media path does not embed a JavaScript runtime.

## Failure behavior

- Control-plane unavailable: existing WebRTC session continues; new sessions wait with backoff.
- Heartbeat fails: device appears offline after 45 seconds; credentials are retained.
- Capture revoked/locked: video pauses and a typed status travels over reliable control.
- Capture/encoder task fails: one bounded whole-pipeline restart; a second failure publishes a
  terminal host signal, marks the Convex session failed with an audit event, and closes the peer.
- Data channel closes: all injected input is released immediately.
- Token revoked: the revoke transaction ends active session records immediately; the browser closes
  from the reactive state update, and the agent closes every peer on its next session poll (bounded
  by `network.poll_milliseconds`) before exiting successfully.
- TURN unavailable: direct paths still work; the UI reports reachability rather than spinning.
