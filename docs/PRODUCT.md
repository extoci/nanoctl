# Product

nanoctl is a small, self-hosted remote desktop for devices on your Tailscale network.
The host agent serves the controller itself; opening its URL is the complete browser
workflow. A native Windows desktop shell provides a polished entry point and Labs without
replacing that streaming workflow yet. See [ADR 0004](adr/0004-tailnet-native.md).

## Experience

- Install Tailscale, run the nanoctl installer, open the printed host URL.
- Auto-connect to the host; see its name, platform and your authenticated tailnet identity.
- H.264 video fits the viewport. Pick a display, toggle remote input, enter fullscreen,
  or enable keyboard lock where the secure-context browser API is available.
- Release remote input with Ctrl+Alt+Shift+Esc. View-only users cannot enable input.
- Inspect resolution, decoded FPS, fractional kbps, RTT and ICE route. Unknown rates
  show a dash until sampled; a static desktop can have sparse frames and very low traffic.
- Socket loss gets bounded reconnect. Another viewer taking over gets a clear message
  and explicit Reconnect action. Only one session is active per host.

## Labs

Desktop experiments are opt-in, independently toggleable and platform-specific. They stay off
by default so alpha users can track current builds without inheriting unfinished behavior.
Client experiments affect the desktop shell; host experiments are written to the host agent
configuration and applied after the agent restarts.

## Boundaries

There is no hosted nanoctl account, device inventory, control plane, enrollment service,
STUN/TURN deployment or web application to operate. Tailscale provides reachability
and identity, including its own encrypted DERP fallback. The default owner policy can
be extended with users, tags or control/view-only capability grants.

Native HTTPS uses `tailscale cert` after HTTPS is enabled in the tailnet admin console;
HTTP fallback remains encrypted by WireGuard but loses ordinary secure-context APIs.
Tailnet networking must permit both signaling and WebRTC traffic.

Text clipboard sync is available to controlling viewers when enabled and supported by the host.
System audio streams to controlling and view-only viewers on Linux (PulseAudio/PipeWire-pulse)
and Windows (WASAPI loopback); browser playback starts muted. macOS audio is unsupported.
This alpha does not provide file transfer, multiple simultaneous
viewers, unattended sign-in-screen access, UAC secure-desktop control or a public
internet gateway. Hardware encoder availability and OS capture/input consent vary;
see [platforms](PLATFORMS.md). Do not equate a successful local test with physical
cross-platform or real-tailnet release qualification.
