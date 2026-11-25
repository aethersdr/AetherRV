#!/bin/bash
# Install the Peplink READ-ONLY web-admin user's credentials for AetherRV on the Pi.
#
#   bash tools/peplink_install_readonly_user.sh [router] [pi-ssh-host]
#   defaults: PEPLINK_ROUTER / AETHERRV_HOST from packaging/local.env (gitignored)
#   needs curl and jq here; the Pi stores the result with aetherrv-setup store-secrets
#
# First enable the read-only user in the Peplink web admin (System > Admin Security:
# "Read-only User Name" / "Read-only User Password"). This script then:
#   - prompts for that username/password (password not echoed, never written locally)
#   - logs in once via POST /api/login and REFUSES unless the router grants GET only (no POST)
#   - writes peplink_ro_user / peplink_ro_password to the Pi's /etc/aetherrv/secrets.toml
#     (root:aetherrv 0640) and logs out
set -euo pipefail
[ -f "$(dirname "$0")/../packaging/local.env" ] && . "$(dirname "$0")/../packaging/local.env"
ROUTER=${1:-${PEPLINK_ROUTER:-}}
PI=${2:-${AETHERRV_HOST:-}}
[ -n "$ROUTER" ] && [ -n "$PI" ] || { echo "usage: $0 <router-ip> <pi-ssh-host>  (or set them in packaging/local.env)" >&2; exit 2; }
API="https://$ROUTER/api"
JAR=$(mktemp); OUT=$(mktemp)
trap 'curl -sk -b "$JAR" -X POST "$API/logout" >/dev/null 2>&1 || true; rm -f "$JAR" "$OUT"' EXIT

read -rp "Peplink READ-ONLY username: " RO_USER
read -rsp "Peplink READ-ONLY password: " RO_PASS; echo

# the password reaches curl on stdin (printf is a shell builtin), never on a command line
printf '%s' "$RO_PASS" | jq -Rs --arg u "$RO_USER" '{username: $u, password: .}' \
  | curl -sk -c "$JAR" -H 'Content-Type: application/json' --data-binary @- "$API/login" > "$OUT"
[ "$(jq -r .stat "$OUT" 2>/dev/null)" = ok ] || { echo "login failed: $(jq -r '"\(.code) \(.message)"' "$OUT" 2>/dev/null || echo unparsable)"; exit 1; }
echo "granted permission: $(jq -c '.response.permission // {}' "$OUT")"
if jq -e '(.response.permission.POST // false) as $p | $p != false and $p != 0' "$OUT" >/dev/null; then
  echo "REFUSING: this account can write (POST). Use the router's read-only user."; exit 1
fi
code=$(curl -sk -b "$JAR" -o /dev/null -w '%{http_code}' "$API/info.location")
echo "test read info.location -> http $code"

printf 'peplink_ro_user=%s\npeplink_ro_password=%s\n' "$RO_USER" "$RO_PASS" \
  | ssh -o BatchMode=yes "$PI" 'sudo /opt/aetherrv-rs/bin/aetherrv-setup store-secrets'
unset RO_PASS
echo "Installed on $PI:/etc/aetherrv/secrets.toml (root:aetherrv 0640). Logging out."
