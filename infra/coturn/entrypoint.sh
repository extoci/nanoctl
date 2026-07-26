#!/bin/sh
set -eu

fail() {
  printf 'nanoctl coturn: %s\n' "$1" >&2
  exit 1
}

case "${TURN_REALM:-}" in
  "" | *[!A-Za-z0-9.-]*) fail "TURN_REALM must be a DNS name" ;;
esac

case "${TURN_EXTERNAL_IP:-}" in
  "" | *[!0-9A-Fa-f:./]*) fail "TURN_EXTERNAL_IP must be a public IP or public/private mapping" ;;
esac

case "${TURN_LISTEN_IP:-}" in
  "" | *[!0-9A-Fa-f:.]*) fail "TURN_LISTEN_IP must be a local IP address" ;;
esac

case "${TURN_RELAY_IP:-}" in
  "" | *[!0-9A-Fa-f:.]*) fail "TURN_RELAY_IP must be a local IP address" ;;
esac

auth_file=/run/secrets/turn_auth_secret
[ -r "$auth_file" ] || fail "turn_auth_secret is not readable"
auth_secret=$(cat "$auth_file")
[ "${#auth_secret}" -ge 32 ] || fail "turn_auth_secret must contain at least 32 characters"
[ "${#auth_secret}" -le 256 ] || fail "turn_auth_secret must contain at most 256 characters"
case "$auth_secret" in
  *'
'*) fail "turn_auth_secret must be one line" ;;
esac

runtime_config=/tmp/turnserver.conf
umask 077
cp /etc/nanoctl/turnserver.conf "$runtime_config"
{
  printf 'realm=%s\n' "$TURN_REALM"
  printf 'external-ip=%s\n' "$TURN_EXTERNAL_IP"
  printf 'listening-ip=%s\n' "$TURN_LISTEN_IP"
  printf 'relay-ip=%s\n' "$TURN_RELAY_IP"
  printf 'static-auth-secret=%s\n' "$auth_secret"
  if [ "${TURN_TLS_ENABLED:-1}" = "1" ]; then
    [ -r /run/secrets/turn_fullchain.pem ] || fail "turn_fullchain.pem is not readable"
    [ -r /run/secrets/turn_privkey.pem ] || fail "turn_privkey.pem is not readable"
    printf 'cert=/run/secrets/turn_fullchain.pem\n'
    printf 'pkey=/run/secrets/turn_privkey.pem\n'
    printf 'alt-tls-listening-port=443\n'
  else
    printf 'no-tls\n'
    printf 'no-dtls\n'
  fi
} >>"$runtime_config"
unset auth_secret

exec turnserver -c "$runtime_config" "$@"
