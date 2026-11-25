#!/bin/bash
# Reboot → usable UI on the BENCH Pi, timed from the workstation (the firmware part is invisible to the Pi itself).
#   UIBENCH_HOST=<bench> harness/uibench/boot-time.sh <wpe|wpe-launcher|tauri|chromium> [rust|rust-kiosk|rust-light|rust-old]   (the order: boot-measure.sh)
# Only when the bench is free: it reboots it. Steps: arm boot-measure.sh for the runtime; note the bench's clock
# offset; `systemctl reboot`; ping it every 100 ms and poll its gateway (:8080) every 100 ms; after the boot, read the
# bench's own figures (boot-measure.sh report: kernel → gateway → runtime launch → first usable frame, on the kernel's
# clock) and place them on the workstation's clock; disarm. Prints, all on the workstation's clock:
#   last ping reply before the reboot → first ping reply after → gateway answering → kernel start (from the bench's
#   wall clock minus its uptime, offset-corrected) → first usable frame,
# and two firmware estimates, both upper bounds: kernel start − the last ping reply (it includes the end of the
# shutdown after the network went down), and kernel start − the previous boot's last journal entry (bench clock).
# Results: harness/out/uibench/boot-<runtime>[-<order>]-<date>.json (gitignored).
set -euo pipefail
HOST=${UIBENCH_HOST:?set UIBENCH_HOST to the ssh host of the bench Pi, never the van}
RT=${1:?runtime: wpe|wpe-launcher|tauri|chromium}
ORDER=${2:-}
# UIBENCH_REBOOT_FF=1: no shutdown at all, to split the firmware from the shutdown's tail. The heaters must be idle;
# then aetherrv.target, the CAN helpers and the simulators are stopped, the exact wall time is written and synced
# (/var/tmp/uibench-boot/reboot-ff.txt) and `systemctl reboot -ff` calls reboot(2) at once. The firmware is then
# that timestamp → the kernel's start, both on the bench's clock.
FF=${UIBENCH_REBOOT_FF:-0}
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
if ! ssh -o BatchMode=yes "$HOST" '! ip -br link | grep -qE "^(hat0|hat1)\b"'; then echo "refused: $HOST has hat0/hat1, it is the van" >&2; exit 3; fi
# its wired LAN address (not a tunnel: the timings must not include a VPN coming up), else the ssh host name
ADDR=$(ssh -o BatchMode=yes "$HOST" "ip -4 -br addr show end0 2>/dev/null | awk '{print \$3}' | cut -d/ -f1" || true)
[ -n "$ADDR" ] || ADDR=$(ssh -G "$HOST" | awk '/^hostname /{print $2}')
# the timing runs on this side, in the benchmark tool built for the workstation (now, while nothing is armed)
cargo build --release --quiet --manifest-path "$ROOT/utils/uibench/Cargo.toml"
BIN=${CARGO_TARGET_DIR:-$ROOT/utils/uibench/target}/release/aetherrv-uibench
# whatever happens below (a hung ssh, ^C), the unit must not stay armed for the next boot
# shellcheck disable=SC2088,SC2064  # ~ expands on the bench; HOST is fixed
trap "ssh -o BatchMode=yes -o ConnectTimeout=5 '$HOST' '~/uibench/harness/boot-measure.sh disarm' || echo 'DISARM FAILED: run boot-measure.sh disarm on the bench' >&2" EXIT
# preflight: the bench must answer ping and its gateway must answer on the LAN address, or the timings are lost
ping -c 1 -W 1 "$ADDR" > /dev/null || { echo "refused: $ADDR does not answer ping" >&2; exit 4; }
curl -sf -m 2 -o /dev/null "http://$ADDR:8080/" || { echo "refused: the gateway does not answer on http://$ADDR:8080/" >&2; exit 4; }
# shellcheck disable=SC2088
ssh "$HOST" "~/uibench/harness/boot-measure.sh arm $RT $ORDER"
mkdir -p "$ROOT/harness/out/uibench"
OUTF="$ROOT/harness/out/uibench/boot-$RT${ORDER:+-$ORDER}-$(date +%Y%m%d-%H%M%S).json"
if [ "$FF" = 1 ]; then
  # shellcheck disable=SC2088
  ssh "$HOST" '/opt/aetherrv-rs/src/packaging/heaters-idle.sh' || { echo "refused: the heaters are not idle" >&2; exit 6; }
  ssh "$HOST" 'sudo systemctl stop aetherrv.target "aetherrv-can-aux@*" "aetherrv-sim-*"; systemctl list-units --plain --no-legend "aetherrv*" --state=active | awk "{print \$1}" | paste -sd" "'
fi
"$BIN" boot-time "$HOST" "$ADDR" "$OUTF" "$FF"
# (the EXIT trap disarms the unit)
