# nanoctl

nanoctl is a fast, headless remote-desktop service with a browser controller. The host agent runs
as an operating-system service on Windows, macOS, or Linux. The web application authenticates with
[Shoo](https://shoo.dev), stores control-plane state in Convex, and negotiates an encrypted WebRTC
connection directly to the host. A TURN relay is used only when a direct connection cannot be made.

This repository is the v1 rewrite. The original LAN demo remains available on the `master` branch.

## Repository

| Path                   | Purpose                                                                      |
| ---------------------- | ---------------------------------------------------------------------------- |
| `apps/web`             | Next.js dashboard, browser controller, Convex schema/functions               |
| `crates/nanoctl-agent` | Native headless capture, input, WebRTC, and service process                  |
| `packages/protocol`    | Versioned signaling and control protocol                                     |
| `infra/coturn`         | Reference TURN deployment                                                    |
| `docs`                 | Product, architecture, security, platform, protocol, testing, and operations |

## Prerequisites

- Bun 1.3.14 or newer
- Rust 1.96 or newer
- a Convex project
- a Shoo-supported HTTPS origin
- a public TURN service for production

## Development

```sh
bun install
cp apps/web/.env.example apps/web/.env.local
bun run convex:dev
bun run dev
```

In another terminal:

```sh
cargo run --manifest-path crates/nanoctl-agent/Cargo.toml -- doctor
cargo run --manifest-path crates/nanoctl-agent/Cargo.toml -- enroll ABCDE-FGHJK-MNPQR-STVWX
cargo run --manifest-path crates/nanoctl-agent/Cargo.toml -- run
```

Run all repository checks with `bun run check`.

Host registration and removal instructions are in [docs/SETUP.md](docs/SETUP.md).

## Security posture

nanoctl is designed for device-owner-authorized access. It does not hide its installation, bypass
OS consent, capture a locked secure desktop without supported system APIs, or provide an arbitrary
remote shell. Media and input travel over DTLS-SRTP/SCTP. The control plane sees device metadata,
session metadata, and encrypted transport addresses, but not unencrypted desktop content.

Read [docs/SECURITY.md](docs/SECURITY.md) before deploying.

## Current deployment boundary

The source is production-oriented, but a release is not considered supported until its platform
package has passed the physical-machine matrix in [docs/TESTING.md](docs/TESTING.md). VM-only tests
cannot validate hardware encoders, macOS TCC prompts, Windows secure desktop behavior, Wayland
portals, or hostile NAT traversal.
