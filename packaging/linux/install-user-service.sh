#!/bin/sh
set -eu

binary_path=${1:?usage: install-user-service.sh /path/to/nanoctl}
[ "$(id -u)" -ne 0 ] || {
  printf '%s\n' "Install nanoctl as the enrolled graphical user, not root." >&2
  exit 1
}
[ ! -e "$HOME/.local/bin/nanoctl" ] &&
  [ ! -e "$HOME/.config/systemd/user/nanoctl.service" ] || {
  printf '%s\n' "nanoctl is already installed. Uninstall or update it explicitly." >&2
  exit 1
}
install -Dm755 "$binary_path" "$HOME/.local/bin/nanoctl"
if ! "$HOME/.local/bin/nanoctl" doctor; then
  rm -f "$HOME/.local/bin/nanoctl"
  exit 1
fi
install -Dm644 "$(dirname "$0")/nanoctl.service" \
  "$HOME/.config/systemd/user/nanoctl.service"
systemctl --user daemon-reload
systemctl --user enable --now nanoctl.service
printf '%s\n' "nanoctl user service installed and started."
