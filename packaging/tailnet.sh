#!/bin/sh

check_tailnet() {
  tailscale_cli="$(command -v tailscale || true)"
  if [ -z "$tailscale_cli" ] && [ -x /Applications/Tailscale.app/Contents/MacOS/Tailscale ]; then
    tailscale_cli=/Applications/Tailscale.app/Contents/MacOS/Tailscale
  fi
  if [ -z "$tailscale_cli" ] || ! "$tailscale_cli" status --json 2>/dev/null |
    grep -Eq '"BackendState"[[:space:]]*:[[:space:]]*"Running"'; then
    printf '%s\n' 'Warning: install/start Tailscale and sign in: https://tailscale.com/download' >&2
  fi
}

remove_legacy_credential() {
  config_output="$("$1" paths 2>/dev/null || true)"
  case "$config_output" in
    config=/?*) config_path="${config_output#config=}" ;;
    *) return 0 ;;
  esac
  [ -f "$config_path" ] || return 0
  legacy_device_id="$(awk -F '["\047]' '/^[[:space:]]*device_id[[:space:]]*=/ {print $2; exit}' "$config_path" 2>/dev/null || true)"
  [ -n "$legacy_device_id" ] || return 0
  if [ "$(uname -s)" = Darwin ]; then
    security delete-generic-password -s dev.nanoctl.agent -a "$legacy_device_id" >/dev/null 2>&1 || true
  elif command -v keyctl >/dev/null 2>&1; then
    for ring in @s @u "$(keyctl get_persistent "$(id -u)" 2>/dev/null || true)"; do
      [ -n "$ring" ] || continue
      key="$(keyctl search "$ring" user "keyring-rs:$legacy_device_id@dev.nanoctl.agent" 2>/dev/null || true)"
      [ -z "$key" ] || keyctl invalidate "$key" >/dev/null 2>&1 || true
    done
  fi
}

print_tailnet_url() {
  if url_output="$("$1" url --wait)"; then
    printf '%s\n%s\n' 'Open on a device connected to Tailscale:' "$url_output"
  else
    printf '%s\n' 'URL unavailable yet; retry nanoctl url --wait after Tailscale is running.' >&2
  fi
}
