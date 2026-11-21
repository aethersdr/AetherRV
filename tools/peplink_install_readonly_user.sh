#!/bin/bash
# Install the Peplink READ-ONLY web-admin user's credentials for AetherRV on the Pi.
#
#   bash tools/peplink_install_readonly_user.sh [router] [pi-ssh-host]
#   defaults: PEPLINK_ROUTER / AETHERRV_HOST from deploy/local.env (gitignored)
#
# First enable the read-only user in the Peplink web admin (System > Admin Security:
# "Read-only User Name" / "Read-only User Password"). This script then:
#   - prompts for that username/password (password not echoed, never written locally)
#   - logs in once via POST /api/login and REFUSES unless the router grants GET only (no POST)
#   - writes peplink_ro_user / peplink_ro_password to the Pi's /etc/aetherrv/secrets.toml
#     (root:aetherrv 0640) and logs out
set -euo pipefail
[ -f "$(dirname "$0")/../deploy/local.env" ] && . "$(dirname "$0")/../deploy/local.env"
ROUTER=${1:-${PEPLINK_ROUTER:-}}
PI=${2:-${AETHERRV_HOST:-}}
[ -n "$ROUTER" ] && [ -n "$PI" ] || { echo "usage: $0 <router-ip> <pi-ssh-host>  (or set them in deploy/local.env)" >&2; exit 2; }
API="https://$ROUTER/api"
JAR=$(mktemp); OUT=$(mktemp)
trap 'curl -sk -b "$JAR" -X POST "$API/logout" >/dev/null 2>&1 || true; rm -f "$JAR" "$OUT"' EXIT

read -rp "Peplink READ-ONLY username: " RO_USER
read -rsp "Peplink READ-ONLY password: " RO_PASS; echo

printf '%s' "$RO_PASS" | python3 -c 'import json,sys; print(json.dumps({"username": sys.argv[1], "password": sys.stdin.read()}))' "$RO_USER" \
  | curl -sk -c "$JAR" -H 'Content-Type: application/json' --data-binary @- "$API/login" > "$OUT"
python3 - "$OUT" <<'PY' || exit 1
import json, sys
d = json.load(open(sys.argv[1]))
if d.get("stat") != "ok":
    print("login failed:", d.get("code"), d.get("message")); sys.exit(1)
perm = (d.get("response") or {}).get("permission") or {}
print("granted permission:", perm)
if perm.get("POST"):
    print("REFUSING: this account can write (POST). Use the router's read-only user."); sys.exit(1)
PY
code=$(curl -sk -b "$JAR" -o /dev/null -w '%{http_code}' "$API/info.location")
echo "test read info.location -> http $code"

printf '%s\n%s\n' "$RO_USER" "$RO_PASS" | ssh -o BatchMode=yes "$PI" 'sudo python3 -c "
import sys, json
user, pw = sys.stdin.read().split(\"\n\")[:2]
f = \"/etc/aetherrv/secrets.toml\"
try:
    lines = [l for l in open(f).read().splitlines() if not l.startswith((\"peplink_ro_user\", \"peplink_ro_password\"))]
except FileNotFoundError:
    lines = []
lines += [\"peplink_ro_user = \" + json.dumps(user), \"peplink_ro_password = \" + json.dumps(pw)]
open(f, \"w\").write(\"\n\".join(lines) + \"\n\")
" && sudo chown root:aetherrv /etc/aetherrv/secrets.toml && sudo chmod 640 /etc/aetherrv/secrets.toml'
unset RO_PASS
echo "Installed on $PI:/etc/aetherrv/secrets.toml (root:aetherrv 0640). Logging out."
