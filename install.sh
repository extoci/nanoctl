#!/bin/sh
set -eu

REPOSITORY="${NANOCTL_REPOSITORY:-extoci/nanoctl}"
VERSION="${NANOCTL_VERSION:-latest}"
CONTROL_PLANE="${NANOCTL_CONTROL_PLANE:-https://nanoctl.vercel.app}"

fail() {
  printf 'nanoctl installer: %s\n' "$*" >&2
  exit 1
}

[ "$(id -u)" -ne 0 ] || fail "run this as the desktop user, not root"
command -v uname >/dev/null 2>&1 || fail "uname is required"

case "$(uname -s):$(uname -m)" in
  Linux:x86_64|Linux:amd64)
    platform=linux
    target=linux-x64
    ;;
  Linux:aarch64|Linux:arm64)
    platform=linux
    target=linux-arm64
    ;;
  Darwin:x86_64)
    platform=macos
    target=macos-x64
    ;;
  Darwin:arm64|Darwin:aarch64)
    platform=macos
    target=macos-arm64
    ;;
  *) fail "unsupported platform: $(uname -s) $(uname -m)" ;;
esac

if [ "$VERSION" = latest ]; then
  base_url="https://github.com/$REPOSITORY/releases/latest/download"
else
  base_url="https://github.com/$REPOSITORY/releases/download/$VERSION"
fi

temporary="$(mktemp -d 2>/dev/null || mktemp -d -t nanoctl)"
staging=
rollback=0
cleanup() {
  status=$?
  trap '' HUP INT TERM
  if [ "$status" -ne 0 ] && [ "$rollback" -eq 1 ]; then
    if restore_install; then
      printf 'nanoctl installer: rolled back the failed installation\n' >&2
    else
      printf 'nanoctl installer: rollback incomplete; remaining files are in %s\n' "$staging" >&2
      staging=
    fi
  fi
  if [ -n "$staging" ]; then
    rm -rf "$staging"
  fi
  rm -rf "$temporary"
  exit "$status"
}
trap cleanup EXIT
trap 'exit 1' HUP INT TERM

if [ -n "${NANOCTL_BINARY:-}" ]; then
  [ -x "$NANOCTL_BINARY" ] || fail "NANOCTL_BINARY is not executable: $NANOCTL_BINARY"
  cp "$NANOCTL_BINARY" "$temporary/nanoctl"
else
  command -v curl >/dev/null 2>&1 || fail "curl is required"
  asset="nanoctl-$target"
  printf 'Downloading nanoctl for %s...\n' "$target"
  curl -fL --proto '=https' --tlsv1.2 "$base_url/$asset" -o "$temporary/nanoctl"
  curl -fL --proto '=https' --tlsv1.2 "$base_url/$asset.sha256" -o "$temporary/nanoctl.sha256"

  expected="$(awk '{print $1}' "$temporary/nanoctl.sha256")"
  case "$expected" in
    *[!0-9a-fA-F]*|'') fail "release checksum is invalid" ;;
  esac
  if command -v sha256sum >/dev/null 2>&1; then
    actual="$(sha256sum "$temporary/nanoctl" | awk '{print $1}')"
  elif command -v shasum >/dev/null 2>&1; then
    actual="$(shasum -a 256 "$temporary/nanoctl" | awk '{print $1}')"
  elif command -v openssl >/dev/null 2>&1; then
    actual="$(openssl dgst -sha256 "$temporary/nanoctl" | awk '{print $NF}')"
  else
    fail "sha256sum, shasum, or openssl is required"
  fi
  [ "$expected" = "$actual" ] || fail "release checksum did not match"
fi
install_dir="$HOME/.local/bin"
binary_path="$install_dir/nanoctl"
mkdir -p "$install_dir"
staging="$(mktemp -d "$install_dir/.nanoctl.install.XXXXXX")"
candidate="$staging/nanoctl"
cp "$temporary/nanoctl" "$candidate"
chmod 755 "$candidate"
"$candidate" --version >/dev/null || fail "candidate binary could not run"

config_output="$("$candidate" paths)" || fail "candidate could not report its configuration path"
case "$config_output" in
  *'
'*) fail "candidate returned an invalid configuration path" ;;
  config=/?*) config_path="${config_output#config=}" ;;
  *) fail "candidate returned an invalid configuration path" ;;
esac

if [ ! -f "$config_path" ]; then
  setup_code="${NANOCTL_ENROLL_CODE:-}"
  if [ -z "$setup_code" ]; then
    [ -r /dev/tty ] || fail "set NANOCTL_ENROLL_CODE when installing without a terminal"
    printf 'Setup code: ' >/dev/tty
    IFS= read -r setup_code </dev/tty
  fi
  [ -n "$setup_code" ] || fail "setup code cannot be empty"
  "$candidate" enroll "$setup_code" --control-plane "$CONTROL_PLANE"
fi

service_was_running=0
if [ "$platform" = linux ]; then
  command -v systemctl >/dev/null 2>&1 || fail "systemd is required on Linux"
  systemctl --user show-environment >/dev/null || fail "cannot connect to the systemd user manager"
  service_path="$HOME/.config/systemd/user/nanoctl.service"
  case "$(systemctl --user is-active nanoctl.service || true)" in
    active|activating|reloading) service_was_running=1 ;;
  esac
  cat >"$staging/service" <<EOF
[Unit]
Description=nanoctl remote desktop agent
After=network-online.target graphical-session.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=$binary_path run
Restart=on-failure
RestartSec=5s
NoNewPrivileges=true
PrivateTmp=true
UMask=0077

[Install]
WantedBy=default.target
EOF
else
  service_domain="gui/$(id -u)"
  service_target="$service_domain/dev.nanoctl.agent"
  service_path="$HOME/Library/LaunchAgents/dev.nanoctl.agent.plist"
  launchctl print "$service_domain" >/dev/null || fail "cannot connect to the launchd user domain"
  if launchctl print "$service_target" >/dev/null 2>&1; then
    [ -f "$service_path" ] || fail "existing launchd service has no installer-managed plist"
    service_was_running=1
  fi
  escaped_binary="$(printf '%s' "$binary_path" | sed 's/&/\\&amp;/g; s/</\\&lt;/g; s/>/\\&gt;/g')"
  cat >"$staging/service" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>dev.nanoctl.agent</string>
  <key>ProgramArguments</key>
  <array><string>$escaped_binary</string><string>run</string></array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
  <key>ProcessType</key><string>Interactive</string>
  <key>ThrottleInterval</key><integer>5</integer>
</dict>
</plist>
EOF
  chmod 600 "$staging/service"
fi

path_exists() {
  [ -e "$1" ] || [ -L "$1" ]
}

restore_path() {
  if path_exists "$1"; then
    mv -f "$1" "$2"
  else
    rm -f "$2"
  fi
}

restore_install() {
  result=0
  if [ "$start_attempted" -eq 1 ]; then
    if [ "$platform" = linux ]; then
      systemctl --user stop nanoctl.service || result=1
    elif launchctl print "$service_target" >/dev/null 2>&1; then
      launchctl bootout "$service_target" || result=1
    fi
  fi
  if [ "$enable_attempted" -eq 1 ]; then
    systemctl --user disable nanoctl.service || result=1
  fi
  if [ "$files_changed" -eq 1 ]; then
    restore_path "$staging/previous-binary" "$binary_path" || result=1
    restore_path "$staging/previous-service" "$service_path" || result=1
    if [ "$platform" = linux ]; then
      systemctl --user daemon-reload || result=1
    fi
  fi
  if [ "$result" -eq 0 ] && [ "$service_was_running" -eq 1 ]; then
    if [ "$platform" = linux ]; then
      systemctl --user start nanoctl.service || result=1
    elif ! launchctl print "$service_target" >/dev/null 2>&1; then
      launchctl bootstrap "$service_domain" "$service_path" || result=1
    fi
  fi
  return "$result"
}

service_pid() {
  if [ "$platform" = linux ]; then
    systemctl --user show --property=MainPID --value nanoctl.service
  else
    job="$(launchctl print "$service_target")" || return 1
    printf '%s\n' "$job" | awk '$1 == "pid" && $2 == "=" { print $3; exit }'
  fi
}

wait_for_service() {
  # Check the same process survives startup; a restart policy can mask a crash.
  for attempt in 1 2 3; do
    pid="$(service_pid)" || return 1
    case "$pid" in
      ''|0|*[!0-9]*)
        if [ "$attempt" -lt 3 ]; then
          sleep 1 || return 1
        fi
        ;;
      *)
        sleep 2 || return 1
        current_pid="$(service_pid)" || return 1
        [ "$current_pid" = "$pid" ]
        return "$?"
        ;;
    esac
  done
  return 1
}

mkdir -p "${service_path%/*}"
for path in "$binary_path" "$service_path"; do
  [ ! -d "$path" ] || fail "expected a file, found a directory: $path"
done
if path_exists "$binary_path"; then
  cp -pP "$binary_path" "$staging/previous-binary"
fi
if path_exists "$service_path"; then
  cp -pP "$service_path" "$staging/previous-service"
fi

files_changed=0
start_attempted=0
enable_attempted=0
rollback=1
if [ "$platform" = linux ]; then
  if [ "$service_was_running" -eq 1 ] || path_exists "$service_path"; then
    systemctl --user stop nanoctl.service
  fi
elif [ "$service_was_running" -eq 1 ]; then
  launchctl bootout "$service_target"
fi

files_changed=1
mv -f "$candidate" "$binary_path"
mv -f "$staging/service" "$service_path"
if [ "$platform" = linux ]; then
  systemctl --user daemon-reload
  start_attempted=1
  systemctl --user start nanoctl.service
else
  start_attempted=1
  launchctl bootstrap "$service_domain" "$service_path"
  launchctl kickstart -k "$service_target"
fi
wait_for_service || fail "candidate service did not stay running"
if [ "$platform" = linux ] && ! path_exists "$staging/previous-service"; then
  enable_attempted=1
  systemctl --user enable nanoctl.service
fi
rollback=0

printf '\nnanoctl is installed, enrolled, and running.\n'
printf 'Run this installer again at any time to update.\n'
case ":$PATH:" in
  *":$install_dir:"*) ;;
  *)
    case "${SHELL:-}" in
      */zsh) shell_profile="$HOME/.zshrc" ;;
      */bash) shell_profile="$HOME/.bashrc" ;;
      *) shell_profile="" ;;
    esac
    if [ -n "$shell_profile" ]; then
      # shellcheck disable=SC2016 # Keep HOME and PATH literal for the user's future shell.
      path_line='export PATH="$HOME/.local/bin:$PATH"'
      if ! grep -F "$path_line" "$shell_profile" >/dev/null 2>&1; then
        {
          printf '\n# nanoctl\n'
          printf '%s\n' "$path_line"
        } >>"$shell_profile"
      fi
      printf 'Added nanoctl to PATH for new terminals (%s).\n' "$shell_profile"
    else
      printf 'Add %s to PATH to run nanoctl directly.\n' "$install_dir"
    fi
    ;;
esac
if [ "$platform" = macos ]; then
  printf 'macOS may ask you to allow Screen Recording and Accessibility for nanoctl.\n'
fi
