#!/bin/sh
set -eu
# shellcheck source=packaging/tailnet.sh
. "$(dirname "$0")/../tailnet.sh"

binary_path=${1:?usage: install-user-service.sh /path/to/nanoctl}
[ "$(id -u)" -ne 0 ] || {
  printf '%s\n' "Install nanoctl as the graphical user, not root." >&2
  exit 1
}
if [ -e "$HOME/.local/bin/nanoctl" ] ||
  [ -e "$HOME/.config/systemd/user/nanoctl.service" ]; then
  printf '%s\n' "nanoctl is already installed. Uninstall or update it explicitly." >&2
  exit 1
fi
install -Dm755 "$binary_path" "$HOME/.local/bin/nanoctl"
if ! "$HOME/.local/bin/nanoctl" config >/dev/null; then
  rm -f "$HOME/.local/bin/nanoctl"
  exit 1
fi
install -d -m 700 "$HOME/.config/nanoctl"
install -Dm644 "$(dirname "$0")/nanoctl.service" \
  "$HOME/.config/systemd/user/nanoctl.service"
systemctl --user daemon-reload
systemctl --user enable --now nanoctl.service
printf '%s\n' "nanoctl user service installed and started."
remove_legacy_credential "$HOME/.local/bin/nanoctl"
check_tailnet
print_tailnet_url "$HOME/.local/bin/nanoctl"
