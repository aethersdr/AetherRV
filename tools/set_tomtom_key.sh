#!/usr/bin/env bash
# Store the TomTom API key (Weather-map traffic overlay) in the Pi's secrets file and restart the backend.
# Run from the laptop: ./tools/set_tomtom_key.sh   — the key is typed on the Pi with no echo and never
# lands in shell history or on a command line. Replaces any earlier tomtom_api_key line.
set -euo pipefail
HOST="${1:-aetherpiii}"
ssh -t "$HOST" 'set -e
read -rsp "TomTom API key: " K; echo
[ -n "$K" ] || { echo "no key entered"; exit 1; }
F=/etc/aetherrv/secrets.toml
sudo sed -i "/^tomtom_api_key/d" "$F"
printf "\ntomtom_api_key = \"%s\"\n" "$K" | sudo tee -a "$F" >/dev/null
sudo -u aetherrv python3 -c "import tomllib; tomllib.load(open(\"$F\", \"rb\"))" && echo "secrets file OK"
sudo systemctl restart aetherrv && echo done'
