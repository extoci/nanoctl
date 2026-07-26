# Security and privacy

## Security goals

- Only the authenticated owner can enumerate or initiate access to a device.
- Enrollment is explicit, short-lived, single-use, and resistant to guessing.
- A database disclosure does not reveal usable device credentials.
- Media and control are confidential and integrity-protected in transit.
- The agent exposes no inbound TCP listener and accepts input only inside an authorized live session.
- Disconnect, expiry, revocation, and parse failure fail closed.
- Logs and telemetry cannot reconstruct desktop content, clipboard content, auth tokens, or SDP.

## Threat model

We defend against internet scanners, guessed enrollment codes, replayed signals, malicious browser
content outside the nanoctl origin, another nanoctl user, a passive network observer, a compromised
TURN relay, and accidental over-broad service permissions.

We do not claim to defend a machine from its local administrator/root, a compromised OS or browser,
a malicious owner controlling their own enrolled machine, screen observation at either endpoint, or
Shoo/Convex account takeover. Account MFA is inherited from the Google account brokered by Shoo.

## Authentication and authorization

Shoo tokens are accepted only after ES256 signature, exact issuer, exact origin audience, expiry, and
subject validation by Convex. Client-side identity fields never make authorization decisions.
Because Shoo labels itself “super early WIP,” all Shoo-specific code stays behind one adapter. A
production deployment must monitor issuer/JWKS availability and have a documented migration path.

Enrollment codes contain 100 bits of random entropy, expire after ten minutes, and are consumed
atomically. The endpoint is additionally rate-limited at the edge. The resulting agent token is 256
random bits, returned once over TLS, stored in the OS credential facility, and represented in Convex
only by SHA-256. Revocation changes the indexed hash, ends active session records in the same
transaction, closes the browser peer reactively, and causes the agent to close all peers and stop
when its next bounded session poll is rejected.

Session authorization is `(owner subject, device id, session id, expiry, role)`. IDs alone are never
capabilities. Signal sequence numbers are scoped by sender. Replays are ignored. SDP, candidates,
clipboard, names, and reasons have explicit size limits.

## Transport

Browser-to-control-plane traffic requires TLS. WebRTC uses ICE + DTLS, SRTP for media, and SCTP over
DTLS for data. TURN relays encrypted packets and receives ephemeral credentials. Production enables
TLS 1.3 where supported, HSTS at the web edge, no mixed content, and a restrictive CSP.

WebRTC encryption does not independently authenticate the remote machine’s human-readable name.
The authenticated signaling channel binds the session and SDP fingerprints. A future high-assurance
mode may add device identity signatures over fingerprints.

## Agent hardening

- Run as a dedicated service identity where platform capture permits; split privileged input helper
  from network process when required.
- Store config read-only to the service account/administrators and secrets in Keychain, Credential
  Manager/DPAPI, or Secret Service/kernel keyring.
- Never put tokens or pairing codes in process lists after enrollment. Installer input is passed by
  protected pipe or UI field.
- Bind diagnostics IPC to a local protected socket/pipe. Do not listen publicly.
- Use signed, reproducible packages and a signed update manifest. Updates are HTTPS, signature
  verified, atomic, and rollback-capable.
- Drop frames and messages at bounded queues; reject decompression bombs and oversized JSON before
  allocation.
- Validate advanced config against a deny-by-default schema.

## OS-specific boundaries

Windows input must not bypass the secure desktop. macOS capture and input require Screen Recording
and Accessibility TCC grants. Linux Wayland uses PipeWire/xdg-desktop-portal and compositor-approved
input mechanisms; unsupported compositors are view-only. X11 input is inherently broad and the UI
warns accordingly. Locked-session behavior follows OS policy and is surfaced to the controller.

## Web hardening

The app sets no-sniff, frame denial, referrer, and permissions headers. Production adds a nonce-based
CSP permitting only the application, Convex, and Shoo endpoints. Remote video is rendered directly
to a media element, never injected HTML. Clipboard is opt-in and capped. Browser key handling is
active only while the video owns focus and always preserves an emergency escape chord.

## Abuse controls

Rate-limit enrollment by IP, code digest, and account; session creation by account/device; agent
authentication failures by IP; and signaling by session/role. Alert on repeated code failures,
geographically implausible session requests, new device enrollment, credential revocation, and
unusual relay volume. Never silently auto-enroll.

## Secret inventory

| Secret              | Location                            | Rotation                   |
| ------------------- | ----------------------------------- | -------------------------- |
| Agent token         | OS credential store; hash in Convex | revoke/re-enroll           |
| Shoo signing key    | Shoo; public JWKS cached by Convex  | issuer-managed             |
| TURN auth secret    | Convex/edge secret + coturn         | quarterly / incident       |
| Convex deploy key   | CI secret store                     | least privilege / incident |
| Package signing key | offline or managed signing service  | documented ceremony        |

## Reporting

Do not open public issues containing tokens, SDP, ICE candidates, private IPs, crash dumps, or
screenshots. Security reports should include affected version, platform, reproducibility, and impact.
Until a private reporting channel is configured, deployment owners must publish one before launch.
