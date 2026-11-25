#!/usr/bin/env bash
# Store the TomTom API key (Weather-map traffic overlay) in the Pi's secrets file and restart the weather service,
# which reads it at start. Run from the laptop: ./tools/set_tomtom_key.sh — the key is typed on the Pi with no echo
# and never lands in shell history or on a command line. Replaces any earlier tomtom_api_key line.
set -euo pipefail
# Pi: the argument, or AETHERRV_HOST from packaging/local.env (gitignored; see packaging/local.env.example).
[ -f "$(dirname "$0")/../packaging/local.env" ] && . "$(dirname "$0")/../packaging/local.env"
HOST="${1:-${AETHERRV_HOST:-}}"
[ -n "$HOST" ] || { echo "usage: tools/set_tomtom_key.sh <pi-ssh-host>   (or set AETHERRV_HOST in packaging/local.env)" >&2; exit 2; }
ssh -t "$HOST" 'set -e
read -rsp "TomTom API key: " K; echo
[ -n "$K" ] || { echo "no key entered"; exit 1; }
printf "tomtom_api_key=%s\n" "$K" | sudo /opt/aetherrv-rs/bin/aetherrv-setup store-secrets
sudo systemctl restart aetherrv-weather && echo done'
