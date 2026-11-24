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
#
# There is deliberately no generic "run" subcommand. The host comes from deploy/local.env (AETHERRV_HOST).
set -euo pipefail
cd "$(dirname "$0")/.."
. deploy/local.env
HOST=${AETHERRV_HOST:?AETHERRV_HOST missing in deploy/local.env}
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
  *)
    sed -n '2,19p' "$0"; exit 2 ;;
esac
