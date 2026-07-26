# ADR 0002: Convex control plane with TURN fallback

Status: accepted.

Convex stores identity-bound devices, sessions, audit metadata, and transient signaling. It does not
carry media. WebRTC prefers direct ICE paths. Standards-compliant TURN is mandatory in production as
an encrypted relay fallback.

This keeps normal latency and bandwidth cost low while allowing operation behind symmetric NAT,
CGNAT, UDP-blocking firewalls, and enterprise proxies. “Never relay” would be faster only for the
connections it can establish and would violate the reliability promise.
