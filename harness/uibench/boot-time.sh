#!/bin/bash
# Reboot → usable UI on the BENCH Pi, timed from the workstation (the firmware part is invisible to the Pi itself).
#   UIBENCH_HOST=<bench> harness/uibench/boot-time.sh <wpe|wpe-launcher|tauri|chromium|python> [rust|rust-light|rust-old|python]   (the order: boot-measure.sh)
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
RT=${1:?runtime: wpe|wpe-launcher|tauri|chromium|python}
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
python3 - "$HOST" "$ADDR" "$OUTF" "$FF" <<'EOF'
import json, subprocess, sys, time, urllib.request
host, addr, outf, ff = sys.argv[1:5]
def ssh(cmd, t=20):
    try:
        return subprocess.run(["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=3", host, cmd], capture_output=True, text=True, timeout=t)
    except subprocess.TimeoutExpired:
        return subprocess.CompletedProcess(cmd, 255, "", "timeout")
def offset():
    """bench clock − workstation clock, from the ssh round trip with the smallest delay"""
    best = None
    for _ in range(5):
        t1 = time.time(); r = ssh("date +%s.%N"); t2 = time.time()
        if r.returncode == 0 and (best is None or t2 - t1 < best[0]):
            best = (t2 - t1, float(r.stdout) - (t1 + t2) / 2)
    return best
def ping(w="0.15"):
    return subprocess.run(["ping", "-c", "1", "-W", w, addr], capture_output=True).returncode == 0
def gateway():
    try: return urllib.request.urlopen(f"http://{addr}:8080/", timeout=0.5).status == 200
    except Exception: return False
off0 = offset()
t_cmd = time.time()
if ff == "1":     # the exact wall time, on disk, then reboot(2) at once: no shutdown at all
    ssh("t=$(date +%s.%N); echo $t | sudo tee /var/tmp/uibench-boot/reboot-ff.txt >/dev/null; sync; sudo systemctl reboot -ff", t=5)
else:
    ssh("sudo systemctl reboot", t=5)
last_up = t_cmd
while ping():                                   # still answering (the shutdown); ~0.1 s resolution
    last_up = time.time(); time.sleep(0.05)
down = time.time()
first_ping = gw = None
while first_ping is None and time.time() - down < 300:   # the kernel answers ping before any service: ~0.15 s resolution
    t = time.time()
    if ping(): first_ping = t
while first_ping and gw is None and time.time() - first_ping < 120:   # then the gateway: a closed port is refused at once
    t = time.time()
    if gateway(): gw = t
    else: time.sleep(0.05)
# the bench's own figures (wait for the boot measurement to have written its result)
rep = None
for _ in range(90):
    r = ssh("~/uibench/harness/boot-measure.sh report")
    if r.returncode == 0: rep = r.stdout; break
    time.sleep(2)
off1 = offset()
bench = json.loads(rep.strip().splitlines()[-1]) if rep else {}
# the firmware on the bench's own clock: the previous boot's last journal entry ("Journal stopped", late in the
# shutdown) → this kernel's start; an upper bound (the last shutdown steps run after the journal stops)
jl = ssh("journalctl -b -1 -o short-unix -n 1 --no-pager").stdout.split()
try: journal_stop = float(jl[0])
except (IndexError, ValueError): journal_stop = None
ffr = ssh("cat /var/tmp/uibench-boot/reboot-ff.txt; sudo rm -f /var/tmp/uibench-boot/reboot-ff.txt").stdout.split() if ff == "1" else []
try: ff_t = float(ffr[0])
except (IndexError, ValueError): ff_t = None
o = off1[1] if off1 else (off0[1] if off0 else 0.0)
k = bench.get("kernel_start_epoch")
k_ws = k - o if k else None
usable_ws = k_ws + bench["first_usable_frame_s"] if k_ws and bench.get("first_usable_frame_s") else None
res = {
    "reboot_command": t_cmd, "last_ping_reply": last_up, "first_ping_reply_after": first_ping, "gateway_answering": gw,
    "kernel_start": k_ws, "first_usable_frame": usable_ws,
    "clock_offset_s": o, "offset_rtt_s": (off1 or off0 or [None])[0],
    "firmware_upper_bound_s": (k_ws - last_up) if k_ws else None,
    "previous_journal_stop_epoch_bench": journal_stop,
    "firmware_from_journal_s": (k - journal_stop) if k and journal_stop else None,
    "reboot_ff": ff == "1", "reboot_ff_epoch_bench": ff_t,
    "firmware_from_reboot_ff_s": (k - ff_t) if k and ff_t else None,     # reboot(2) at once → the kernel's start
    "down_to_gateway_s": (gw - last_up) if gw else None,
    "down_to_usable_s": (usable_ws - last_up) if usable_ws else None,
    "kernel_to_usable_s": bench.get("first_usable_frame_s"),
    "bench_report": rep,
    # the kernel must start between the last reply before the reboot and the first after it, else a clock stepped
    "kernel_start_plausible": bool(k_ws and first_ping and last_up < k_ws < first_ping),
    "polling": "ping every ~0.1 s while up, ~0.15 s while down; gateway every ~0.05 s after the first ping",
}
json.dump(res, open(outf, "w"), indent=1)
f = lambda x: "–" if x is None else f"{x:.2f} s"
rel = lambda t: None if t is None else t - last_up
print(rep or "(no report from the bench)")
print(f"on the workstation's clock, from the last ping reply before the reboot (clock offset {o:+.3f} s, rtt {f(res['offset_rtt_s'])}):")
for name, t in (("first ping reply", first_ping), ("kernel start", k_ws), ("gateway answering", gw), ("first usable frame", usable_ws)):
    print(f"  {name:22s} {f(rel(t))}")
print(f"kernel start between the last and first ping replies: {res['kernel_start_plausible']}")
print(f"firmware from the previous boot's last journal entry to the kernel start (bench clock): {f(res['firmware_from_journal_s'])}")
if ff == "1":
    print(f"firmware from the immediate reboot(2) to the kernel start (bench clock): {f(res['firmware_from_reboot_ff_s'])}")
print(f"firmware (upper bound: includes the shutdown's end) {f(res['firmware_upper_bound_s'])}; kernel → usable {f(res['kernel_to_usable_s'])}; "
      f"down → usable {f(res['down_to_usable_s'])}  ({outf})")
EOF
# (the EXIT trap disarms the unit)
