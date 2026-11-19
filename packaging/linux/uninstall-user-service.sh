#!/bin/sh
set -eu
# shellcheck source=packaging/tailnet.sh
. "$(dirname "$0")/../tailnet.sh"

[ "$(id -u)" -ne 0 ] || {
  printf '%s\n' "Uninstall nanoctl as the graphical user, not root." >&2
  exit 1
}
systemctl --user disable --now nanoctl.service 2>/dev/null || true
if [ -x "$HOME/.local/bin/nanoctl" ]; then
  remove_legacy_credential "$HOME/.local/bin/nanoctl"
fi
rm -f "$HOME/.config/systemd/user/nanoctl.service" "$HOME/.local/bin/nanoctl"
systemctl --user daemon-reload
printf '%s\n' "nanoctl user service removed; configuration preserved."
