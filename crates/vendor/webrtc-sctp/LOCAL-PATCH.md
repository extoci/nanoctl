# nanoctl v4 SCTP patch

The library sources, manifest, examples, and license files come from the published
`webrtc-sctp` 0.17.3 crate. Its archive SHA-256 is
`190e2985eddccc80353de2441c6878ef84ffe2e12beef5aba2e9d4207ffff1e5`.
The archive identifies upstream commit `a3c3f4dff44f2c935be3535ae94330770cd50dbd`.

The local patch sets the minimum SCTP retransmission timeout to 200 ms. See
[webrtc-rs issue #938](https://github.com/webrtc-rs/webrtc/issues/938).
It also cancels an old timeout callback when a SACK restarts the timer while
holding the association lock. The bad-network gate records T3 timeout events
with browser frame timestamps so transport stalls can be correlated.

On a T3 timeout, expire timed DATA before building retransmissions. Advance
FORWARD TSN in the same timeout so expired video does not use the reduced
congestion window. See [RFC 3758 section 4.1](https://www.rfc-editor.org/rfc/rfc3758.html#section-4.1).

Request immediate SACKs on timeout retransmissions and new sends under the
one-packet congestion window. The sender can resume after one RTT when the peer
honors the request. See [RFC 7053 section 4.2](https://www.rfc-editor.org/rfc/rfc7053.html#section-4.2).

Update the audit entry in `.github/workflows/ci.yml` when changing the upstream
version. Before removing the patch, verify the timer setting, cancellation
behavior, and the v4 bad-network gate with the new dependency.
