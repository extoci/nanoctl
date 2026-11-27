# nanoctl v4 SCTP patch

The library sources, manifest, examples, and license files come from the published
`webrtc-sctp` 0.17.3 crate. Its archive SHA-256 is
`190e2985eddccc80353de2441c6878ef84ffe2e12beef5aba2e9d4207ffff1e5`.
The archive identifies upstream commit `a3c3f4dff44f2c935be3535ae94330770cd50dbd`.

The local patch sets the minimum SCTP retransmission timeout to 200 ms. The
initial timeout, round-trip time calculation, and exponential backoff retain
their upstream values. See [webrtc-rs issue #938](https://github.com/webrtc-rs/webrtc/issues/938).

Update the audit entry in `.github/workflows/ci.yml` when changing the upstream
version. Remove this patch once a supported upstream dependency exposes the
timer setting and the normal release build passes the v4 bad-network gate.
