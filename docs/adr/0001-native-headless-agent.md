# ADR 0001: Native headless host agent

Status: accepted.

The host is implemented as a native Rust service with per-user platform companions where required.
Bun is the workspace/runtime for TypeScript tooling and Next development, not the privileged capture
runtime.

A JavaScript-only daemon would still require unsigned or opaque native addons for capture, hardware
encoding, WebRTC, credential storage, and input. Owning these boundaries in a small native agent
improves startup, packaging, memory control, queue bounds, and OS API access. It does add Rust and
platform build complexity, which is accepted because hiding it behind FFmpeg subprocesses would not
remove the complexity from the product.
