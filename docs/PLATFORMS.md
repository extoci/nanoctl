# Platform implementation

## Windows

Target Windows 10 version 1903 or newer and Windows 11. The backend keeps xcap's
Windows.Graphics.Capture recorder alive for the session, retains only the newest RGBA frame, and
uses enigo's Windows input backend. It enumerates only
hardware Media Foundation NV12-to-H.264 transforms, unlocks their asynchronous event protocol,
requires low-latency mode, and falls back to OpenH264 unless `quality.encoder = "hardware"` makes
hardware mandatory. The H.264 subtype guarantees Annex-B samples with interleaved SPS/PPS, and the
agent validates each access unit before RTP. RGBA-to-NV12 and the hardware-surface boundary still
copy through system memory. Direct D3D11 capture-surface import remains a physical performance gate
and must not be advertised until it passes on signed release hardware.

The current package registration is a headless, non-elevated per-user Scheduled Task because
Session 0 cannot capture the user desktop or use the enrolling user's Credential Manager entry. The
binary is administrator-owned under Program Files while configuration access is restricted to the
agent identity, SYSTEM, and local administrators. Before changing state, the installer compares the
configuration owner SID to the elevated identity and rejects elevation through another
administrator. A future supervisor/helper split may add a LocalSystem supervisor, but the
capture/input process must remain in the authorized user session. UAC secure desktop, elevated
applications, and Windows sign-in screens remain inaccessible.

## macOS

Target macOS 14+. The portable backend keeps xcap's native recorder alive, retains only the newest
RGBA frame, and uses enigo. The media backend prefers a real-time, no-frame-reordering VideoToolbox
H.264 session, converts its AVCC output to WebRTC Annex-B with explicit SPS/PPS, and falls back to
OpenH264 unless `quality.encoder = "hardware"` makes hardware mandatory. Capture still copies the
xcap RGBA frame into a BGRA IOSurface; direct ScreenCaptureKit IOSurface handoff remains a release
performance gate and must not be advertised until its signed physical test passes. Input requires
Accessibility and capture requires Screen Recording permission. The current package installs a
LaunchAgent because TCC permission is tied to an interactive code identity/session.

The setup tool opens the correct System Settings pages and verifies grants. It cannot click consent
for the user. Fast user switching and lock behavior are tested explicitly.

## Linux

The backend keeps xcap's PipeWire or X11 recorder alive and retains only the newest RGBA frame. It
prefers the ChromeOS `cros-codecs` constrained-baseline H.264 VA-API encoder, converts RGBA to NV12,
honors driver-reported surface pitches, and falls back to bounded OpenH264 unless
`quality.encoder = "hardware"` makes hardware mandatory. Building requires libva headers; runtime
requires a working `/dev/dri/renderD*` device and a driver that advertises constrained-baseline
slice encoding. The current VA-API boundary uploads system-memory NV12 into a hardware surface;
direct PipeWire DMA-BUF import remains a physical performance gate.

RemoteDesktop portal input and reusable portal restore tokens remain future compositor integration
work. Portal restore tokens are non-secret configuration and may still require renewed consent.

X11 cannot provide strong per-application input isolation, so diagnostics must label it as a weaker
environment. The current package is a systemd user service so capture, input, and the credential
store share the enrolled graphical identity. Release binaries target Ubuntu 24.04 or another
distribution with glibc 2.39+ and a current PipeWire runtime. Supported reference desktops are
GNOME and KDE; other compositors may be view-only.

## Packaging contract

Packages are built for x64 and arm64, signed per platform, and include:

- agent/service binaries and license notices;
- service manifests with restart backoff and sane resource limits;
- a `doctor` command and uninstall path;
- no embedded deployment secrets or static TURN password;
- atomic update support with signature verification and rollback.

Uninstall stops the service, revokes local credentials where reachable, removes binaries and service
registration, and preserves only an optional redacted diagnostic log after explicit consent.
