# Research record

Research was performed against primary project documentation in July 2026.

## Shoo

Shoo describes itself as an early-work-in-progress Google authentication broker. Its browser flow
uses mandatory S256 PKCE, derives a pairwise subject per application origin, and returns ES256
identity tokens. The issuer is `https://shoo.dev`, the audience is `origin:<canonical-origin>`, and
public keys are exposed through standard JWKS. Its React package supplies a Convex auth adapter.

Implication: Shoo is usable as the requested identity provider, but it is isolated behind
`apps/web/lib/shoo.ts`. Convex performs JWT validation. Authorization uses only the verified
`identity.subject`, never browser-decoded claims. Production readiness depends on Shoo availability,
revocation behavior, and a migration plan because the service itself warns it is early.

Sources:

- <https://docs.shoo.dev/docs/how-it-works>
- <https://docs.shoo.dev/docs/server-verification>
- <https://docs.shoo.dev/docs/convex>
- <https://shoo.dev/privacy>

## Convex

Convex supports custom JWT/OIDC issuers and validates identity for WebSocket subscriptions and
functions. Its Next.js client supports client-side authentication through
`ConvexProviderWithAuth`. Public deployment endpoints require authorization checks inside every
function. HTTP actions are appropriate for authenticated non-browser agents when their own
credentials are verified.

Implication: Convex stores ownership and ephemeral signaling and exposes a narrow agent API. It does
not relay media. Agent tokens are independent random credentials because a headless service does not
represent an interactive Shoo session.

Sources:

- <https://docs.convex.dev/auth/overview>
- <https://docs.convex.dev/auth/advanced/custom-jwt>
- <https://docs.convex.dev/client/nextjs/app-router/>

## TypeScript 7, Bun, and oxc

TypeScript 7.0 is the native compiler and exposes a `tsc` binary but no programmatic compiler API.
Tools embedding the old API must temporarily run beside TypeScript 6 or earlier. Bun supports
TypeScript 7 and recommends bundler resolution, preserved modules, strict checking, and explicit Bun
types. Next currently invokes the legacy compiler API during its production build.

Implication: `@typescript/native` 7.0 is the authoritative application/shared-package `typecheck`
command. A legacy `typescript` package exists only for Next’s internal build hook and Convex's
generated-function checker; it does not determine application diagnostics. oxfmt and oxlint replace
Prettier/ESLint.

Sources:

- <https://devblogs.microsoft.com/typescript/announcing-typescript-7-0/>
- <https://bun.sh/docs/typescript>

## WebRTC and NAT traversal

WebRTC encrypts media with SRTP negotiated by DTLS and data channels with SCTP over DTLS. ICE checks
host, server-reflexive, and relay candidates. STUN alone cannot traverse all NAT/firewall
combinations; TURN is required for dependable internet operation. A TURN server forwards encrypted
packets and can consume bandwidth comparable to the media rate in both directions.

Implication: direct ICE is preferred, but TURN-over-UDP/TCP/TLS is a first-class production
dependency. Signaling rows are short-lived because candidates contain addressing metadata. Input is
split by delivery semantics rather than sharing one reliable queue with pointer motion.

Sources:

- <https://www.w3.org/TR/webrtc/>
- <https://www.rfc-editor.org/rfc/rfc8445>
- <https://www.rfc-editor.org/rfc/rfc8656>
- <https://www.rfc-editor.org/rfc/rfc8827>

## Native H.264 encoding

Apple VideoToolbox accepts IOSurface-backed pixel buffers and exposes real-time, expected-frame-rate,
bitrate, keyframe-interval, profile, and frame-reordering controls. Its compressed H.264 sample data
uses AVCC length-prefixed NAL units; WebRTC transport requires an explicit Annex-B conversion and
the format description’s parameter sets. The selected Rust bindings retain the CoreMedia sample
buffer while those parameter-set pointers are copied.

Implication: macOS can use VideoToolbox immediately after copying the portable xcap RGBA frame into
a BGRA IOSurface. This moves H.264 compression to hardware but is not yet a zero-copy capture path.

ChromeOS `cros-codecs` provides a lightweight Linux VA-API H.264 encoder with low-delay prediction,
CBR/CQP controls, forced keyframes, and VA surface import. Its companion `cros-libva` binding is
used directly rather than spawning FFmpeg or GStreamer. Driver support is capability-tested at
runtime; libva availability alone does not imply that H.264 encoding exists. The current capture
API yields RGBA system memory, so v1 converts to NV12 before a pitched VA surface upload. A future
PipeWire DMA-BUF capture path can remove that copy without changing signaling or RTP.
Direct ScreenCaptureKit IOSurface handoff remains a separate physical performance gate.

Sources:

- <https://developer.apple.com/documentation/videotoolbox/vtcompressionsession>
- <https://developer.apple.com/documentation/coremedia/cmvideoformatdescriptiongeth264parametersetatindex(_:parametersetindex:parametersetpointerout:parametersetsizeout:parametersetcountout:nalunitheaderlengthout:)>
- <https://docs.rs/videotoolbox/0.18.1/videotoolbox/>
- <https://docs.rs/apple-cf/0.9.3/apple_cf/iosurface/>
- <https://github.com/chromeos/cros-codecs>
- <https://github.com/intel/libva>
