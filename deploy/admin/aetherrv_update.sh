#!/bin/bash
# AetherRV system update (pacman -Syu), started from Settings → Updates via aetherrv-update.service (root).
# Modelled on the AetherSDR Maestro console's maestro-update.sh. The kernel/boot packages (linux-rpi,
# raspberrypi-bootloader, firmware-raspberrypi) are held in pacman.conf IgnorePkg: the console boots a custom
# kernel, and linux-rpi owns the /boot dtbs + overlays (incl. mcp2515 for the CAN HATs) it boots with.
#
#   /run/aetherrv-update/log     pacman's output, for the progress view
#   /run/aetherrv-update/state   status=running|done|failed, rc=, restart=suggested|no, venv=ok|rebuilt|broken
#   /var/lib/aetherrv-update/last   when the last update finished, and how
set -u
RUN=/run/aetherrv-update LAST=/var/lib/aetherrv-update
mkdir -p "$RUN" "$LAST"; chmod 755 "$RUN" "$LAST"
state() { printf '%s\n' "$@" > "$RUN/state.tmp" && chmod 644 "$RUN/state.tmp" && mv "$RUN/state.tmp" "$RUN/state"; }
: > "$RUN/log"; chmod 644 "$RUN/log"
state "status=running" "started=$(date +%s)"
{
    echo "== Refreshing the package lists and the signing keys"
    pacman -Syy --noconfirm --needed archlinuxarm-keyring archlinux-keyring &&
        echo "== Upgrading the system (kernel/boot packages held)" &&
        pacman -Su --noconfirm
} >> "$RUN/log" 2>&1
rc=$?
restart=no
grep -q -E '^upgrading |^installing ' "$RUN/log" && restart=suggested

# AetherRV's venv points at the system python3: a Python minor upgrade orphans its packages. Check, rebuild.
venv=ok
if ! /opt/aetherrv/.venv/bin/python -c "import aiohttp, can, aetherrv" >/dev/null 2>&1; then
    echo "== AetherRV's Python environment needs rebuilding" >> "$RUN/log"
    if UV_PYTHON_PREFERENCE=only-system uv sync --frozen --no-dev --project /opt/aetherrv -q >> "$RUN/log" 2>&1 &&
       /opt/aetherrv/.venv/bin/python -c "import aiohttp, can, aetherrv" >/dev/null 2>&1; then
        venv=rebuilt; restart=suggested
    else
        venv=broken
    fi
fi
if [ "$rc" -eq 0 ] && [ "$venv" != broken ]; then echo "== Done" >> "$RUN/log"; status=done
else echo "== Stopped (pacman code $rc, venv $venv)" >> "$RUN/log"; status=failed; fi
state "status=$status" "rc=$rc" "restart=$restart" "venv=$venv" "finished=$(date +%s)"
printf 'finished=%s\nstatus=%s\n' "$(date +%s)" "$status" > "$LAST/last.tmp" && chmod 644 "$LAST/last.tmp" && mv "$LAST/last.tmp" "$LAST/last"
exit "$rc"
