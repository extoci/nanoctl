# Host setup

Create a setup code in the nanoctl dashboard, then run the installer as the desktop user that will
share this computer.

Linux and macOS:

```sh
curl -fsSL https://extoci.lol/nanoctl/install | sh
```

Windows PowerShell:

```powershell
irm https://extoci.lol/nanoctl/install | iex
```

The installer selects the correct x64 or arm64 executable, verifies its SHA-256 checksum, asks for
the setup code on first install, and starts the background agent. Run the same command again to
update to the latest release without enrolling again. It also adds `nanoctl` to the current user's
PATH for new terminals.

Windows prints the resolved release and target before downloading, for example
`Downloading nanoctl 2.0.0-alpha.1 for windows-x64...`, verifies that the executable reports that exact
version, and prints the installed command path when the upgrade completes. A failed health-gated
upgrade restores the previous task and executable rather than claiming that the new version is
installed.

If automatic platform selection is undesirable, use
`https://extoci.lol/nanoctl/install.sh` or
`https://extoci.lol/nanoctl/install.ps1` explicitly.

Linux uses a systemd user service, macOS uses a per-user LaunchAgent, and Windows uses a hidden,
non-elevated Scheduled Task launched through the windowless Windows Script Host (`wscript.exe`).
The agent must run in the enrolled interactive user's session so it can access that user's
credential store, screen, and input APIs. It does not open an inbound port. The registration starts
at login and restarts after failures. While idle, the agent does not capture the screen or initialize
a media session; it only performs lightweight control-plane checks while waiting for an authorized
session. Windows runtime
output is written to `%LOCALAPPDATA%\nanoctl\agent.log`; no console window is opened by the task.

On Linux and macOS, the installer checks that the candidate can run and report its configuration
path before stopping the old service. If the candidate fails to start or exits during the startup
check, the installer restores the previous executable and service registration and brings back
the service if it was running before the upgrade.

On Windows, an upgrade preserves the existing configuration and enrollment, migrates an older
explicit `--config` task path when needed, and retains the previous executable until the new task
passes its health checks. A failed upgrade restores the previous binary and task automatically.

Configurations created by an older elevated setup may be owned by `BUILTIN\Administrators`. The
installer detects that legacy ACL before changing the installation. If the same Windows account is
an administrator but is running with a filtered token, it automatically requests UAC approval once,
reruns the exact resolved release, and grants the enrolled user explicit access. It still rejects a
task or configuration belonging to another Windows user.

macOS may ask for Screen Recording and Accessibility. Wayland may ask through its desktop portal.
Complete those prompts as the user who ran the installer.

## Verification

Run:

```sh
nanoctl doctor
nanoctl media-smoke --require-hardware --seconds 30
```

Then confirm the dashboard reports the device online and test a connection from another network.
Use `--json` when collecting a machine-readable acceptance record.

To stop the installed background agent without opening Task Scheduler, launchd, or systemd tools,
run:

```sh
nanoctl stop
```

The command asks the running per-user agent to close active sessions, waits for native capture and
encoder resources to be released, and succeeds if the agent is already stopped. The normal
login-triggered registration remains installed, so it starts again at the next login or reinstall.

Nanoctl 2.0 requires a protocol-v2 agent for new sessions. An older enrolled agent remains visible
while it is being upgraded, and the dashboard explains why Connect is unavailable.

## Advanced and development setup

The installer supports these environment overrides:

- `NANOCTL_VERSION=v2.0.0-alpha.1` installs a specific release instead of `latest`.
- `NANOCTL_REPOSITORY=owner/repository` downloads from another GitHub repository.
- `NANOCTL_CONTROL_PLANE=https://example.convex.site` enrolls against another deployment.
- `NANOCTL_ENROLL_CODE=...` supplies the setup code without an interactive prompt.
- `NANOCTL_BINARY=/path/to/nanoctl` uses a local Unix executable instead of downloading.

## Alpha testing

The first nanoctl 2.0 alpha is a prerelease and must be selected explicitly; stable `latest`
continues to resolve to the latest stable release.

Linux and macOS:

```sh
curl -fsSL https://github.com/extoci/nanoctl/releases/download/v2.0.0-alpha.1/install.sh \
  | NANOCTL_VERSION=v2.0.0-alpha.1 sh
```

Windows PowerShell:

```powershell
$env:NANOCTL_VERSION = "v2.0.0-alpha.1"
irm https://github.com/extoci/nanoctl/releases/download/v2.0.0-alpha.1/install.ps1 | iex
```

The scripts under `packaging/` remain low-level development and removal helpers. Normal
installation and updates should use the one-command installer.
