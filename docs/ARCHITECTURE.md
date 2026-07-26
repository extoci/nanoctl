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

Each platform backend produces GPU-native frames when possible. A bounded latest-frame channel
connects capture to the encoder: under pressure, old frames are dropped rather than accumulating
latency. The encoder emits access units to the WebRTC RTP packetizer. Capture timestamps drive RTP
timestamps. Keyframes are forced on join, display change, decoder request, and recovery.

Congestion control consumes WebRTC statistics and receiver feedback. A controller adjusts bitrate
first, then resolution, then frame rate. It uses hysteresis to avoid oscillation and never allows
encoder output to build an unbounded queue.

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
- Encoder fails: one bounded restart, then session terminates with diagnostics.
- Data channel closes: all injected input is released immediately.
- Token revoked: next agent call stops new sessions; current session ends on policy notification or
  deadline.
- TURN unavailable: direct paths still work; the UI reports reachability rather than spinning.
