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
cleanup() {
  rm -rf "$temporary"
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
chmod 755 "$temporary/nanoctl"
"$temporary/nanoctl" --version >/dev/null

install_dir="$HOME/.local/bin"
binary_path="$install_dir/nanoctl"
mkdir -p "$install_dir"

if [ "$platform" = linux ]; then
  command -v systemctl >/dev/null 2>&1 || fail "systemd is required on Linux"
  systemctl --user stop nanoctl.service 2>/dev/null || true
else
  service_target="gui/$(id -u)/dev.nanoctl.agent"
  launchctl bootout "$service_target" 2>/dev/null || true
fi

candidate="$install_dir/.nanoctl.install.$$"
cp "$temporary/nanoctl" "$candidate"
chmod 755 "$candidate"
mv -f "$candidate" "$binary_path"

config_output="$("$binary_path" paths)"
config_path="${config_output#config=}"
if [ "$config_path" = "$config_output" ]; then
  fail "installed binary returned an invalid configuration path"
fi

if [ ! -f "$config_path" ]; then
  setup_code="${NANOCTL_ENROLL_CODE:-}"
  if [ -z "$setup_code" ]; then
    [ -r /dev/tty ] || fail "set NANOCTL_ENROLL_CODE when installing without a terminal"
    printf 'Setup code: ' >/dev/tty
    IFS= read -r setup_code </dev/tty
  fi
  [ -n "$setup_code" ] || fail "setup code cannot be empty"
  "$binary_path" enroll "$setup_code" --control-plane "$CONTROL_PLANE"
fi

if [ "$platform" = linux ]; then
  service_dir="$HOME/.config/systemd/user"
  mkdir -p "$service_dir"
  cat >"$service_dir/nanoctl.service" <<EOF
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
  systemctl --user daemon-reload
  systemctl --user enable --now nanoctl.service
else
  agent_path="$HOME/Library/LaunchAgents/dev.nanoctl.agent.plist"
  mkdir -p "$HOME/Library/LaunchAgents"
  escaped_binary="$(printf '%s' "$binary_path" | sed 's/&/\\&amp;/g; s/</\\&lt;/g; s/>/\\&gt;/g')"
  cat >"$agent_path" <<EOF
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
  chmod 600 "$agent_path"
  launchctl bootstrap "gui/$(id -u)" "$agent_path"
  launchctl kickstart -k "$service_target"
fi

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
