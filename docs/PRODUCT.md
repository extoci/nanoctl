# Product specification

## Problem

People need reliable access to computers they own without installing a large interactive client,
opening inbound firewall ports, or routing every frame through a vendor. Existing tools often trade
latency for compatibility or simplicity for opaque privileged components.

## v1 promise

nanoctl lets an authenticated owner enroll a computer, see whether it is reachable, and open a
remote desktop session from a modern browser. The normal path is peer-to-peer WebRTC. TURN provides
an encrypted compatibility path. The agent has no persistent graphical UI; setup is a signed
installer plus a short enrollment command or installer field.

v1 includes:

- Windows 11 x64/arm64, macOS 14+ x64/arm64, and current Linux x64/arm64.
- One owner per device. One active controller per device.
- Primary or selected display, pointer, keyboard, wheel, clipboard text, and optional system audio.
- H.264 baseline compatibility, with VP9/AV1 offered only when both endpoints and the encoder support
  them.
- Adaptive bitrate, 30/60 FPS presets, 120 FPS advanced maximum, resolution scaling, and automatic
  recovery after ICE/network changes.
- Device rename, revoke, online state, connection audit, and agent version visibility.
- Unattended access after explicit enrollment and OS permission grant.
- A diagnostics command that never prints credentials.

v1 intentionally excludes:

- file transfer, remote shell, multi-controller collaboration, recording, mobile host capture,
  wake-on-LAN brokerage, organization/RBAC features, and hidden installation;
- bypassing Windows UAC secure desktop, macOS TCC, Wayland portals, lock screens, or other operating
  system boundaries;
- end-to-end identity verification independent of the authenticated control plane. WebRTC media is
  encrypted in transit, while session authorization is brokered by nanoctl.

## Setup experience

1. User signs into the web app with Shoo.
2. User chooses **Add device**, receiving a high-entropy, ten-minute, single-use code.
3. Installer prompts for that code, or the user runs `nanoctl enroll CODE`.
4. Agent exchanges the code for a 256-bit device credential and stores it in the OS credential
   facility with owner/service-only access.
5. Agent guides the user through OS capture/input permissions where required, installs the service,
   starts it, and reports readiness.
6. The device appears online. There is no tray process in v1.

macOS requires an interactive permission bootstrap because Screen Recording and Accessibility
approval cannot be silently granted. Wayland may require a portal chooser on first use. The service
reports these as actionable readiness states rather than pretending the device is online-capable.

## Session experience

The web app creates a short-lived session bound to the authenticated owner and selected device.
The agent polls/streams authorized session requests, then controller and host trickle ICE through
the signaling plane. Once the data channel opens, input becomes active and the browser focuses the
remote canvas. A persistent bar exposes status, latency, display, quality, fullscreen, and end.

Disconnect is fail-closed: input state is released, capture stops, credentials stay resident, and
the session cannot be reused. A brief network interruption triggers ICE restart within the same
session deadline. A new browser tab does not inherit control unless it has the authenticated session.

## Advanced mode

Advanced mode is explicit, local, schema-validated configuration. It permits codec order, encoder,
target/max bitrate, FPS, resolution ceiling, ICE transport policy, STUN/TURN endpoints, network
interface allow/deny lists, latency tuning, logging level, and experimental feature gates.

It does not permit arbitrary command execution, arbitrary FFmpeg arguments, disabling authentication,
exporting secrets, weakening TLS verification, or making the control API listen on a public socket.
Unknown keys fail startup so typos cannot silently weaken behavior.

## Success measures

- input-to-photon median below 80 ms on a wired LAN and below 150 ms on a typical same-region WAN;
- 1080p60 sustained with hardware encoding on supported physical hardware;
- session establishment p95 below 8 seconds when direct and below 12 seconds with TURN;
- automatic recovery from a 5-second network interruption in at least 95% of test runs;
- no unbounded queue between capture, encoder, RTP, signaling, or input injection;
- idle agent memory below 80 MiB and CPU below 0.5% on reference systems;
- no credential material in logs, crash reports, process arguments, or world-readable files.
