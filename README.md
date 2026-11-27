# nanoctl

Fast browser-based remote desktop, served by your own machine over Tailscale.

## Tailnet host picker

The viewer header lists other online nanoctl hosts on your tailnet so you can jump between machines.
Discovery probes each online Tailscale peer on the configured agent port (400ms timeout), caches
results for 15 seconds, and refreshes the picker every 30 seconds. Each destination still applies
its own authentication policy. Set `tailnet.discover_peers = false` to disable discovery.

**3.0 alpha is tailnet-native.** Each Rust agent embeds its static browser viewer.
There is no hosted nanoctl control plane, account, enrollment code, device credential,
STUN or TURN service. Tailscale supplies identity, reachability and WireGuard encryption;
WebRTC carries video and input between your browser and the host.

## Get started

1. [Install Tailscale](https://tailscale.com/download) on the host and controller device,
   sign into your tailnet, and leave Tailscale running.
2. Run the nanoctl installer as the host's interactive desktop user:

   **Linux / macOS**

   ```sh
   curl -fsSL https://github.com/extoci/nanoctl/releases/latest/download/install.sh | sh
   ```

   **Windows PowerShell**

   ```powershell
   irm https://github.com/extoci/nanoctl/releases/latest/download/install.ps1 | iex
   ```

3. Open the printed URL from a browser on your tailnet, normally
   `https://<host>.<tailnet>.ts.net:7337/`. On iPhone, use the full MagicDNS link
   in Safari, not the PC’s IP. To find it again, run `nanoctl url` on the host;
   you don’t need to stop or restart nanoctl.

During prereleases, use the install script from the selected release and set
`NANOCTL_VERSION=v3.0.0-alpha.26` (PowerShell: `$env:NANOCTL_VERSION="v3.0.0-alpha.26"`).
Run the installer again to upgrade. Downloads are checksum-verified; failed startup
restores the previous installation. Existing config is preserved and legacy credentials
are removed best-effort.

Enable HTTPS certificates in the **Tailscale admin console → DNS → HTTPS Certificates**.
The agent runs `tailscale cert` automatically, privately stores and renews the certificate,
and serves native HTTPS. If issuance is unavailable, it warns and falls back to HTTP
over the encrypted tailnet. Some browser APIs then require HTTPS. See [setup](docs/SETUP.md).

## Access

By default only the user who owns the host's Tailscale node can connect and control it.
`tailnet.allow_users`, `tailnet.allow_tags`, or the grant capability
`extoci.lol/cap/nanoctl` extend access; grants can make a viewer view-only.
Tailscale network policy must also permit TCP 7337 and WebRTC UDP traffic.
See [security and sample grants](docs/SECURITY.md).

The viewer auto-connects, displays the host and your tailnet identity, and includes a
display picker, input toggle, fullscreen, secure-context keyboard lock and stream stats.
Only one viewer is active at a time; a newer viewer replaces the older one.
Press **Ctrl+Alt+Shift+Esc** to release remote input.

### Game controllers

For a Windows host, install the official [ViGEmBus driver](https://github.com/nefarius/ViGEmBus/releases)
and restart nanoctl. Pair a controller such as a DualSense with the viewer device (for example,
an iPhone), open nanoctl in Safari over HTTPS, turn on **Gamepad**, and press a button on the
controller so the browser can detect it. The browser must expose the controller through the
standard Gamepad mapping. Windows supports up to four virtual controllers: DualShock 4 for
recognized PlayStation controllers and Xbox 360 for other controllers.

Gamepad forwarding requires permission to control the desktop and only sends input from a
focused viewer. It does not replace Tailscale or require a native iOS helper.

## Develop

```sh
bun install --frozen-lockfile
bun run check
bun run build:agent
```

The release build embeds `apps/viewer/dist`; build the viewer before invoking Cargo directly.
Cargo alone still builds without Bun and serves a placeholder if the viewer was not built.
Native media dependencies and browser checks are described in [testing](docs/TESTING.md).

- [Architecture](docs/ARCHITECTURE.md) · [Protocol](docs/PROTOCOL.md)
- [Operations](docs/OPERATIONS.md) · [Platforms](docs/PLATFORMS.md)
- [Product](docs/PRODUCT.md) · [ADR 0004](docs/adr/0004-tailnet-native.md)

## Optional x264 encoder (GPL)

Default builds and published release jobs do **not** include x264. Their software encoder
remains OpenH264. To opt into system libx264 on Linux, install `libx264-dev` and the normal
[native media dependencies](docs/TESTING.md), then build:

```sh
bun run build:viewer
cargo build --release --features x264 --manifest-path crates/nanoctl-agent/Cargo.toml
```

The `x264` Cargo feature is off by default and includes `media`. **libx264 is GPL:** enabling
this feature is an explicit GPL opt-in, not an MIT-only distribution. Distributing a combined
binary/library requires complying with the applicable GPL terms, including corresponding
source obligations. Dynamic loading does not remove those obligations. Official default
builds and existing release packaging stay non-GPL with respect to x264.

We use a thin C shim compiled against the installed x264 headers instead of reproducing
ABI-sensitive `x264_param_t`/picture structs in Rust. It dynamically loads the system shared
library and resolves the header-versioned `x264_encoder_open_<ABI>` symbol; no x264 source
or static library is vendored or linked into nanoctl. Install the matching runtime library
(for example `libx264-164` on Ubuntu 24.04). Missing/incompatible libraries and initialization
failures fall back to OpenH264; the running library remains loaded until its encoder closes.
The Linux feature-on CI job builds and tests this separately. Other platforms require their
own x264 headers, pkg-config metadata and matching shared library and are not yet validated.

Hardware keeps its existing priority. Once software encoding is selected, x264 is preferred
when available. Set `NANOCTL_SOFTWARE_ENCODER=openh264` to force the OpenH264 software path;
`auto` or an unset variable restores normal selection. Empty/unknown values are errors.
This override does not disable hardware: also set `[quality] encoder = "software"` when
comparing software backends. x264 frame/resolution initialization failures also fall back
to OpenH264 for the rest of the session.

The configuration is ultrafast/zerolatency, up to two sliced threads, CRF 12, I420 8-bit,
Constrained Baseline, no B-frames/lookahead/scheduled IDRs, and Annex-B with repeated SPS/PPS
on requested IDRs. The existing rounded RGB-to-I420 conversion is shared with OpenH264;
VUI signals limited range, BT.709 primaries, sRGB transfer and SMPTE 170M/BT.601 matrix,
with centered 4:2:0 chroma. It intentionally does not copy screen-relay's BT.709 matrix tag.
The existing adaptive controller changes x264's VBV maximum bitrate **and** buffer budget
without resetting CRF, allowing QP to rise to 51 under congestion. Responsiveness, balanced
and quality modes respectively allow one, two and four frame budgets of VBV capacity;
these are rate-control budgets, not queued frames. Existing capture/pacing and viewer
latency-mode behavior are unchanged. See [benchmark results](scripts/bench/RESULTS.md) for
measured quality, bandwidth, CPU, latency and runtime-library costs.

If the bounded encoded-frame handoff drops a reference frame, the sender requests an IDR
and suppresses dependent deltas until a clean recovery IDR arrives. This is required for
long-lived x264 streams without scheduled IDRs and also protects the other backends.

The initial Linux qualification exceeded the historical text/drag PSNR targets but
**failed the +2-ms text input-p50 gate** (+3.54 ms). This remains an experimental
opt-in, not a validated every-axis improvement; no fresh three-product campaign
was run. See the results for paired tables, failed tuning attempts and scope.

Source is MIT licensed; the optional combined x264 build is subject to the GPL as above.
