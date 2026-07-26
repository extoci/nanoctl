# Operations

## Production services

Deploy the Next.js app behind TLS with HSTS and a nonce-based CSP. Deploy Convex production
separately from development and set `APP_ORIGIN` exactly to the canonical HTTPS origin. Configure
Shoo for that origin implicitly through its pairwise audience behavior. Deploy at least two TURN
regions and advertise UDP 3478, TCP 3478, TLS 5349, and TLS/TCP 443 where infrastructure allows.

Use short-lived TURN REST credentials derived from a rotated shared secret. Monitor allocation
failures, relay bandwidth, authentication failures, and region saturation. Capacity planning must
assume a relayed 1080p60 session can consume tens of megabits per second in both ingress and egress.

## Retention

- pairing codes: delete within 24 hours of expiry/consumption;
- signals: delete within one hour of session expiry;
- session metadata: 30 days by default;
- audit events: 90 days by default;
- device heartbeat/status: retained with the device;
- application logs: 14 days, redacted;
- media and clipboard: never stored by control-plane services.

## Observability

Metrics use pseudonymous device/session identifiers and include endpoint success/latency, enrollment
failure category, active sessions, signaling delay, candidate type, connection setup, relay usage,
and agent readiness/version. Do not label metrics with owner IDs, tokens, SDP, candidates, IP
addresses, device names, or clipboard data.

Structured logs contain event type, coarse platform/version, request id, result category, and
redacted pseudonymous ids. Debug mode remains locally enabled for a bounded duration and still
redacts secrets and content.

## Incident priorities

1. Credential/signing compromise: freeze enrollment/updates, rotate secrets, revoke affected
   credentials, publish owner guidance.
2. Cross-account authorization: disable affected functions immediately, preserve redacted audit
   evidence, patch and independently review.
3. TURN abuse: rotate TURN secret, rate-limit allocations, block abusive sources without exposing
   static credentials in clients.
4. Availability: existing P2P sessions may continue; communicate which control-plane operations fail.

## Deployment order

Apply backward-compatible Convex schema/functions first, then web, then agent. Never deploy an agent
requiring a protocol version the control plane does not accept. Destructive schema migrations use a
backfill plus dual-read/write window. Rollback must not re-enable revoked credentials.

The hosted web artifact uses the official OpenNext Cloudflare adapter. Set the production
`NEXT_PUBLIC_CONVEX_URL` in the build environment, run `bun run build:sites`, package `dist` with
the Sites packaging helper, and publish only that exact committed build. `APP_ORIGIN`, `TURN_URLS`,
and `TURN_AUTH_SECRET` belong to the Convex deployment rather than the web runtime. Because the
Convex URL is a browser-visible Next variable, changing it requires rebuilding and publishing a new
web version.

Native release candidates are produced by `.github/workflows/release-candidates.yml` for Linux,
macOS, and Windows on x64 and arm64. The aggregate artifact includes normalized archives, SHA-256
checksums, a CycloneDX SBOM, and provenance attestations. It is not a release: platform signing,
macOS notarization, installer construction, update-manifest signing, and the physical acceptance
matrix remain mandatory gates. Signing must attest the final signed bytes, not reuse the unsigned
candidate attestation.

## Readiness

A production launch is blocked until canonical domains, private security contact, signing
identities, TURN regions, rate limiting, retention jobs, backups, restore test, physical-machine
matrix, and on-call ownership are documented with real values.
