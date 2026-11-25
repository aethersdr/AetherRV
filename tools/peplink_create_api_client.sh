#!/bin/bash
# Create a READ-ONLY Peplink Router API client for AetherRV and install it on the Pi.
#
#   bash tools/peplink_create_api_client.sh [router] [pi-ssh-host]
#   defaults: PEPLINK_ROUTER / AETHERRV_HOST from packaging/local.env (gitignored)
#   needs curl and jq here; the Pi stores the result with aetherrv-setup store-secrets
#
# - Prompts for the Peplink admin password (not echoed; never written to disk or history).
# - POST /api/login -> POST /api/auth.client {"action":"add","name":"AetherRV","scope":"api.read-only"}
#   -> POST /api/logout (Peplink Router API, fw 8.x).
# - Writes peplink_client_id / peplink_client_secret into the Pi's /etc/aetherrv/secrets.toml
#   (root:aetherrv 0640). The secret is never printed.
# - To revoke later: log in, then POST /api/auth.client {"action":"remove","clientId":"<id>"}.
set -euo pipefail
[ -f "$(dirname "$0")/../packaging/local.env" ] && . "$(dirname "$0")/../packaging/local.env"
ROUTER=${1:-${PEPLINK_ROUTER:-}}
PI=${2:-${AETHERRV_HOST:-}}
[ -n "$ROUTER" ] && [ -n "$PI" ] || { echo "usage: $0 <router-ip> <pi-ssh-host>  (or set them in packaging/local.env)" >&2; exit 2; }
API="https://$ROUTER/api"
JAR=$(mktemp); OUT=$(mktemp)
trap 'curl -sk -b "$JAR" -X POST "$API/logout" >/dev/null 2>&1 || true; rm -f "$JAR" "$OUT"' EXIT

read -rp "Peplink admin username [admin]: " USERNAME; USERNAME=${USERNAME:-admin}
read -rsp "Peplink admin password: " PASSWORD; echo

# Login (password goes via stdin to curl, so it never appears in the process list)
printf '%s' "$PASSWORD" | jq -Rs --arg u "$USERNAME" '{username: $u, password: .}' \
  | curl -sk -c "$JAR" -H 'Content-Type: application/json' --data-binary @- "$API/login" > "$OUT"
unset PASSWORD
[ "$(jq -r .stat "$OUT" 2>/dev/null)" = ok ] || { echo "login failed: $(jq -r '"\(.code) \(.message)"' "$OUT" 2>/dev/null || echo unparsable)"; exit 1; }
echo "Logged in as $USERNAME."

# Diagnostics (no secrets): what permission did the login grant, which cookies came back,
# and does the session work for a plain read and for listing clients?
echo "  login permission: $(jq -c '(.response // {}) | .permission // .' "$OUT")"
echo "  cookies set: $(awk '!/^#[^H]/ && NF>=7 {print $6}' "$JAR" | tr '\n' ' ')"
for probe in "status.wan.connection" "auth.client"; do
  code=$(curl -sk -b "$JAR" -o "$OUT.p" -w '%{http_code}' "$API/$probe")
  echo "  GET $probe -> http $code, $(jq -r '"stat=\(.stat) code=\(.code) msg=\(.message)"' "$OUT.p" 2>/dev/null || echo unparsable)"
done
rm -f "$OUT.p"

# Create the read-only client
curl -sk -b "$JAR" -H 'Content-Type: application/json' \
  -d '{"action":"add","name":"AetherRV","scope":"api.read-only"}' -w '\n' "$API/auth.client" > "$OUT"
[ "$(jq -r .stat "$OUT" 2>/dev/null)" = ok ] || { echo "create client failed: $(jq -r '"\(.code) \(.message)"' "$OUT" 2>/dev/null || echo unparsable)"; exit 1; }
[ "$(jq -r .response.scope "$OUT")" = api.read-only ] || { echo "unexpected scope: $(jq -r .response.scope "$OUT")"; exit 1; }
jq -r '.response | "Created client \"\(.name)\" (scope \(.scope)), id \(.clientId[:6])… (secret not shown)"' "$OUT"

# Install on the Pi: replace any previous peplink_client_* lines, keep the rest, 0640 root:aetherrv
jq -r '.response | "peplink_client_id=\(.clientId)\npeplink_client_secret=\(.clientSecret)"' "$OUT" \
  | ssh -o BatchMode=yes "$PI" 'sudo /opt/aetherrv-rs/bin/aetherrv-setup store-secrets'
echo "Installed on $PI:/etc/aetherrv/secrets.toml (root:aetherrv 0640). Logging out."
