#!/bin/sh
set -eu

binary_path=${1:?usage: install-user-service.sh /path/to/nanoctl}
install -Dm755 "$binary_path" "$HOME/.local/bin/nanoctl"
install -Dm644 "$(dirname "$0")/nanoctl.service" \
  "$HOME/.config/systemd/user/nanoctl.service"
systemctl --user daemon-reload
systemctl --user enable --now nanoctl.service
printf '%s\n' "nanoctl user service installed and started."
