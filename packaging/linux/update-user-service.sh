#!/bin/sh
set -eu

manifest_path=${1:?usage: update-user-service.sh /path/to/manifest.json PUBLIC_KEY_BASE64}
public_key=${2:?usage: update-user-service.sh /path/to/manifest.json PUBLIC_KEY_BASE64}
binary_path="$HOME/.local/bin/nanoctl"
activated=0
completed=0

cleanup() {
  if [ "$completed" -eq 1 ]; then
    return
  fi
  systemctl --user stop nanoctl.service 2>/dev/null || true
  if [ "$activated" -eq 1 ]; then
    "$binary_path" rollback-update 2>/dev/null || true
  fi
  systemctl --user start nanoctl.service 2>/dev/null || true
}
trap cleanup EXIT
trap 'exit 130' HUP INT TERM

systemctl --user stop nanoctl.service
"$binary_path" stage-update "$manifest_path" --public-key "$public_key"
"$binary_path" activate-update "$manifest_path" --public-key "$public_key"
activated=1
systemctl --user start nanoctl.service

if "$binary_path" doctor; then
  "$binary_path" commit-update
  completed=1
  trap - EXIT HUP INT TERM
  printf '%s\n' "nanoctl update activated and committed."
  exit 0
fi

printf '%s\n' "nanoctl update failed its health check and was rolled back." >&2
exit 1
