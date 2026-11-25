#!/bin/bash
# READ-ONLY access to the van Pi for the Rust conversion (owner-approved 2026-11-23: copy captures, sample state,
# record device responses with read-only requests). Every remote command is logged with a timestamp to
# harness/out/van-access.log, which is the evidence that nothing was installed, restarted or transmitted.
#
#   tools/van-read.sh identity                 hostname, uptime, interfaces (read-only)
#   tools/van-read.sh state                    AetherRV's own /api/state (JSON to stdout)
#   tools/van-read.sh ls <dir>                 list a directory under /var/lib/aetherrv
#   tools/van-read.sh pull <path> <local-dir>  copy files from /var/lib/aetherrv (rsync, pulling only)
#   tools/van-read.sh mcs7-status <host>       one HTTP GET of the MCS7's /status.xml from the van Pi
#   tools/van-read.sh config                   /etc/aetherrv/aetherrv.toml (no secrets in it)
#   tools/van-read.sh secrets-shape            the secrets file's KEY NAMES only: values are replaced ON THE VAN
#   tools/van-read.sh history-copy <local-dir> pull history.sqlite3 with its -wal/-shm, and settings.json (nothing
#                                              is written on the van; the copy is as consistent as a live WAL pull)
#   tools/van-read.sh audit [since]            the van's own record (boot, unit start times, packages, ssh logins,
#                                              command sources) to show nothing was installed, restarted or sent
#   tools/van-read.sh boot                     the boot path's timings (firewall, network.target, user sessions, the
#                                              kiosk), the bootloader's configuration, and how sound is played
#
# There is deliberately no generic "run" subcommand. The host comes from packaging/local.env (AETHERRV_HOST).
set -euo pipefail
cd "$(dirname "$0")/.."
. packaging/local.env
HOST=${AETHERRV_HOST:?AETHERRV_HOST missing in packaging/local.env}
LOG=harness/out/van-access.log
mkdir -p harness/out

say() { printf '%s %s\n' "$(date -Is)" "$*" >> "$LOG"; }

remote() {           # remote <command string>: logged, then run with no tty and no agent forwarding
  say "ssh $HOST: $1"
  ssh -o BatchMode=yes -o ConnectTimeout=15 -a -T "$HOST" "$1"
}

under_state() {      # only paths below /var/lib/aetherrv, no '..'
  case "$1" in
    /var/lib/aetherrv|/var/lib/aetherrv/*) ;;
    *) echo "refused: $1 is not under /var/lib/aetherrv" >&2; exit 2 ;;
  esac
  case "$1" in *..*) echo "refused: '..' in $1" >&2; exit 2 ;; esac
}

cmd=${1:-}
case "$cmd" in
  identity)
    remote 'hostname; uptime; ip -br link'
    ;;
  state)
    remote 'curl -sS --max-time 10 http://127.0.0.1:8080/api/state'
    ;;
  ls)
    under_state "${2:?dir}"
    remote "sudo -n -u aetherrv ls -la --time-style=+%Y-%m-%dT%H:%M $(printf %q "$2")"
    ;;
  pull)
    under_state "${2:?path}"
    dest=${3:?local dir}
    mkdir -p "$dest"
    say "rsync (pull) $HOST:$2 -> $dest"
    rsync -a --info=stats1 --rsync-path="sudo -n -u aetherrv rsync" "$HOST:$2" "$dest/"
    ;;
  mcs7-status)
    h=${2:?mcs7 host}
    [[ $h =~ ^[0-9.]+$ ]] || { echo "refused: host must be an IP address" >&2; exit 2; }
    remote "curl -sS --max-time 10 --compressed http://$h/status.xml"
    ;;
  config)
    remote 'cat /etc/aetherrv/aetherrv.toml'
    ;;
  secrets-shape)
    remote "sudo -n -u aetherrv sed -E 's/^([A-Za-z0-9_]+)[[:space:]]*=.*/\\1 = \"REDACTED\"/' /etc/aetherrv/secrets.toml"
    ;;
  history-copy)
    dest=${2:?local dir}; mkdir -p "$dest"
    remote 'command -v sqlite3 >/dev/null && echo has-sqlite3 || echo no-sqlite3'
    say "rsync (pull) $HOST:/var/lib/aetherrv/{history.sqlite3*,settings.json} -> $dest"
    rsync -a --rsync-path="sudo -n -u aetherrv rsync" "$HOST:/var/lib/aetherrv/history.sqlite3*" "$HOST:/var/lib/aetherrv/settings.json" "$dest/"
    ;;
  audit)
    # The van's own record, for "nothing was installed, restarted or transmitted": boot time, the Python units'
    # start times and restart counts, no Rust tree or units, package changes, ssh logins and command sources since
    # the conversion began. Reads only.
    since=${2:-2026-11-23 21:00}   # UTC (the conversion began 2026-11-23 21:58 UTC)
    [[ $since =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}( [0-9]{2}:[0-9]{2})?$ ]] || { echo "refused: since must be YYYY-MM-DD[ HH:MM]" >&2; exit 2; }
    remote "TZ=UTC; export TZ; echo \"van boot: \$(uptime -s) (\$(date +%Z))\";
      echo \"model: \$(tr -d '\\0' < /proc/device-tree/model)\"; free -m | head -2;
      systemctl show -p Id -p ActiveEnterTimestamp -p NRestarts aetherrv.service aetherrv-kiosk.service aetherrv-admin.socket;
      test -e /opt/aetherrv-rs && echo '/opt/aetherrv-rs: EXISTS' || echo '/opt/aetherrv-rs: absent';
      echo \"aetherrv unit files: \$(systemctl list-unit-files --no-legend 'aetherrv*' | awk '{print \$1}' | tr '\n' ' ')\";
      grep -E '^\[2026-11-(2[3-9]|30)' /var/log/pacman.log | grep -E '\] (installed|upgraded|removed|reinstalled|downgraded) ' || echo 'pacman: no package changes since 2026-11-23';
      echo 'ssh logins since $since UTC:';
      sudo -n journalctl --since '$since' -o short-iso --no-pager SYSLOG_IDENTIFIER=sshd SYSLOG_IDENTIFIER=sshd-session | grep 'Accepted ' | sed -E 's/^([^ ]+) .* from ([^ ]+) port .*/\1 \2/';
      echo 'commands and settings actions since $since UTC (count, source; none = no lines):';
      sudo -n journalctl -u aetherrv --since '$since' --no-pager | sed -nE 's/.* ([a-z0-9]+ command [a-z_]+=.*|settings action) from ([^ ]+): .*/\2/p' | sort | uniq -c;
      echo \"journal lines since $since UTC: \$(sudo -n journalctl -u aetherrv --since '$since' --no-pager -q | wc -l)\""
    ;;
  boot)
    # For the power-on budget (docs/UI-RUNTIME-BENCHMARK.md §8) and the panel's session: when the firewall,
    # network.target, user sessions and the kiosk started on this boot (CLOCK_MONOTONIC us, i.e. since the kernel
    # started), the bootloader's configuration, and whether a sound server plays the UI's alert chime. Reads only.
    remote "systemctl --version | head -1; systemd-analyze 2>&1 | head -2;
      for u in nftables.service network-pre.target systemd-networkd.service network.target systemd-user-sessions.service user@\$(id -u kiosk 2>/dev/null || echo 0).service aetherrv.service aetherrv-kiosk.service; do
        systemctl show -p Id -p UnitFileState -p ActiveState -p ExecMainStartTimestampMonotonic -p ExecMainExitTimestampMonotonic -p ActiveEnterTimestampMonotonic \$u | tr '\n' ' '; echo; done;
      systemd-analyze blame --no-pager 2>/dev/null | head -12;
      test -s /etc/nftables.conf && echo \"nftables.conf: \$(wc -l < /etc/nftables.conf) lines\" || echo 'nftables.conf: none';
      echo 'bootloader config:'; sudo -n rpi-eeprom-config 2>/dev/null || vcgencmd bootloader_config 2>/dev/null || echo '(not readable)';
      echo 'sound:'; pacman -Q pipewire wireplumber pipewire-pulse pulseaudio gst-plugins-base gst-plugins-good 2>&1;
      pgrep -a -u kiosk 'pipewire|wireplumber|pulseaudio' || echo 'no sound server running as kiosk';
      cat /proc/asound/cards 2>/dev/null || echo 'no sound cards'"
    ;;
  *)
    sed -n '2,23p' "$0"; exit 2 ;;
esac
