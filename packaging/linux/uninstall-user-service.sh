#!/bin/sh
set -eu

[ "$(id -u)" -ne 0 ] || {
  printf '%s\n' "Uninstall nanoctl as the enrolled graphical user, not root." >&2
  exit 1
}
systemctl --user disable --now nanoctl.service 2>/dev/null || true
if [ -x "$HOME/.local/bin/nanoctl" ]; then
  "$HOME/.local/bin/nanoctl" unenroll
fi
rm -f "$HOME/.config/systemd/user/nanoctl.service" "$HOME/.local/bin/nanoctl"
systemctl --user daemon-reload
printf '%s\n' "nanoctl user service and local enrollment removed."
