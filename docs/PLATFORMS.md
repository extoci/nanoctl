# Platform implementation

## Windows

Target Windows 11. The portable v1 backend keeps xcap's Windows.Graphics.Capture recorder alive for
the session, retains only the newest RGBA frame, uses OpenH264 for the browser-compatible baseline
stream, and uses enigo's Windows input backend. This avoids per-frame capture setup but still copies
through system memory. The release performance path is D3D11 into Media Foundation without a
GPU-to-CPU copy; that path must not be advertised until its physical release gate passes.

The current package registration is a headless per-user Scheduled Task because Session 0 cannot
capture the user desktop or use the enrolling user's Credential Manager entry. A future
supervisor/helper split may add a LocalSystem supervisor, but the capture/input process must remain
in the authorized user session. UAC secure desktop and Windows sign-in screens remain inaccessible.

## macOS

Target macOS 14+. The portable backend keeps xcap's native recorder alive, retains only the newest
RGBA frame, and uses OpenH264 plus enigo. The release performance path is ScreenCaptureKit into
VideoToolbox, but it must not be advertised until the signed physical gate passes. Input requires
Accessibility and capture requires Screen Recording permission. The current package installs a
LaunchAgent because TCC permission is tied to an interactive code identity/session.

The setup tool opens the correct System Settings pages and verifies grants. It cannot click consent
for the user. Fast user switching and lock behavior are tested explicitly.

## Linux

The portable backend keeps xcap's PipeWire or X11 recorder alive, retains only the newest RGBA
frame, uses bounded OpenH264 software encoding, and uses enigo input where the session permits it.
The release performance path uses
xdg-desktop-portal/PipeWire with VA-API, and RemoteDesktop portal input where supported. Portal
restore tokens are non-secret configuration and may still require renewed consent.

X11 cannot provide strong per-application input isolation, so diagnostics must label it as a weaker
environment. The current package is a hardened systemd user service so capture, input, and the
credential store share the enrolled graphical identity. Supported reference desktops are GNOME and
KDE on current distributions; other compositors may be view-only.

## Packaging contract

Packages are built for x64 and arm64, signed per platform, and include:

- agent/service binaries and license notices;
- service manifests with restart backoff and sane resource limits;
- a `doctor` command and uninstall path;
- no embedded deployment secrets or static TURN password;
- atomic update support with signature verification and rollback.

Uninstall stops the service, revokes local credentials where reachable, removes binaries and service
registration, and preserves only an optional redacted diagnostic log after explicit consent.
