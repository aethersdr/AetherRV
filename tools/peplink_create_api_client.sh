#!/bin/bash
# Create a READ-ONLY Peplink Router API client for AetherRV and install it on the Pi.
#
#   bash tools/peplink_create_api_client.sh [router] [pi-ssh-host]
#   defaults: PEPLINK_ROUTER / AETHERRV_HOST from deploy/local.env (gitignored)
#
# - Prompts for the Peplink admin password (not echoed; never written to disk or history).
# - POST /api/login -> POST /api/auth.client {"action":"add","name":"AetherRV","scope":"api.read-only"}
#   -> POST /api/logout (Peplink Router API, fw 8.x).
# - Writes peplink_client_id / peplink_client_secret into the Pi's /etc/aetherrv/secrets.toml
#   (root:aetherrv 0640). The secret is never printed.
# - To revoke later: log in, then POST /api/auth.client {"action":"remove","clientId":"<id>"}.
set -euo pipefail
[ -f "$(dirname "$0")/../deploy/local.env" ] && . "$(dirname "$0")/../deploy/local.env"
ROUTER=${1:-${PEPLINK_ROUTER:-}}
PI=${2:-${AETHERRV_HOST:-}}
[ -n "$ROUTER" ] && [ -n "$PI" ] || { echo "usage: $0 <router-ip> <pi-ssh-host>  (or set them in deploy/local.env)" >&2; exit 2; }
API="https://$ROUTER/api"
JAR=$(mktemp); OUT=$(mktemp)
trap 'curl -sk -b "$JAR" -X POST "$API/logout" >/dev/null 2>&1 || true; rm -f "$JAR" "$OUT"' EXIT

read -rp "Peplink admin username [admin]: " USERNAME; USERNAME=${USERNAME:-admin}
read -rsp "Peplink admin password: " PASSWORD; echo

# Login (password goes via stdin to curl, so it never appears in the process list)
python3 -c 'import json,sys; print(json.dumps({"username": sys.argv[1], "password": sys.stdin.read().rstrip("\n")}))' "$USERNAME" <<<"$PASSWORD" \
  | curl -sk -c "$JAR" -H 'Content-Type: application/json' --data-binary @- "$API/login" > "$OUT"
unset PASSWORD
python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); sys.exit(0 if d.get("stat")=="ok" else (print("login failed:", d.get("code"), d.get("message")) or 1))' "$OUT"
echo "Logged in as $USERNAME."

# Diagnostics (no secrets): what permission did the login grant, which cookies came back,
# and does the session work for a plain read and for listing clients?
python3 -c 'import json,sys; r=json.load(open(sys.argv[1])).get("response") or {}; print("  login permission:", json.dumps(r.get("permission", r)))' "$OUT"
echo "  cookies set: $(awk '!/^#[^H]/ && NF>=7 {print $6}' "$JAR" | tr '\n' ' ')"
for probe in "status.wan.connection" "auth.client"; do
  code=$(curl -sk -b "$JAR" -o "$OUT.p" -w '%{http_code}' "$API/$probe")
  echo "  GET $probe -> http $code, $(python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); print("stat=%s code=%s msg=%s" % (d.get("stat"), d.get("code"), d.get("message")))' "$OUT.p" 2>/dev/null || echo unparsable)"
done
rm -f "$OUT.p"

# Create the read-only client
curl -sk -b "$JAR" -H 'Content-Type: application/json' \
  -d '{"action":"add","name":"AetherRV","scope":"api.read-only"}' -w '\n' "$API/auth.client" > "$OUT"
python3 - "$OUT" <<'PY' || exit 1
import json, sys
d = json.load(open(sys.argv[1]))
if d.get("stat") != "ok":
    print("create client failed:", d.get("code"), d.get("message")); sys.exit(1)
r = d["response"]
assert r.get("scope") == "api.read-only", r.get("scope")
print(f"Created client '{r.get('name')}' (scope {r.get('scope')}), id {r['clientId'][:6]}… (secret not shown)")
PY

# Install on the Pi: replace any previous peplink_* lines, keep the rest, 0640 root:aetherrv
python3 -c 'import json,sys; r=json.load(open(sys.argv[1]))["response"]; print(f"peplink_client_id = \"{r[\"clientId\"]}\"\npeplink_client_secret = \"{r[\"clientSecret\"]}\"")' "$OUT" \
  | ssh -o BatchMode=yes "$PI" 'sudo sh -c "f=/etc/aetherrv/secrets.toml; touch \$f; grep -v \"^peplink_client_\" \$f > \$f.new || true; cat >> \$f.new; mv \$f.new \$f; chown root:aetherrv \$f; chmod 640 \$f"'
echo "Installed on $PI:/etc/aetherrv/secrets.toml (root:aetherrv 0640). Logging out."
