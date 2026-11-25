#!/bin/bash
# Bench deploys without the Pi's 13-minute native build: cross-compile a COMMITTED revision on the workstation
# (rustup's aarch64-unknown-linux-gnu target, clang + lld, linked against a sysroot copied from the bench itself, so
# glibc, libgcc and libdbus are the bench's own), then sync the source and the binaries and run packaging/install.sh
# there, as packaging/push.sh does. No containers. Refuses a host with hat0/hat1 (a console, the van) unless --cutover
# is given (owner, 2026-11-24: the van's deploys may be cross-compiled too); then the host's glibc must be the one in
# the sysroot, and install.sh runs with --cutover (installs next to what runs, starts nothing).
#   harness/bench/cross-push.sh sysroot <bench>                  copy the bench's headers and libraries (once, and after a pacman -Syu)
#   harness/bench/cross-push.sh push <rev> <bench>               build <rev> and install it on <bench>; nothing is started
#   harness/bench/cross-push.sh push <rev> <console> --cutover   the same on a console (the van); nothing is started
#   harness/bench/cross-push.sh build <rev>                      build only
# Work files in harness/out/cross/ (gitignored): sysroot/, target/ (incremental across revisions), export/.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
W=$ROOT/harness/out/cross
SYSROOT=${AETHERRV_SYSROOT:-$W/sysroot}
T=aarch64-unknown-linux-gnu
not_van() { if ssh -o BatchMode=yes "$1" 'ip -br link' | grep -qE '^(hat0|hat1)\b'; then echo "refused: $1 has hat0/hat1, it is the van" >&2; exit 3; fi; }
case "${1:-}" in
sysroot)
  H=${2:?usage: cross-push.sh sysroot <bench>}; not_van "$H"
  mkdir -p "$SYSROOT/usr"
  rsync -a --delete "$H":/usr/include/ "$SYSROOT/usr/include/"
  # libraries only: no kernel modules, firmware, Python, browsers or GPU drivers (not linked by anything here)
  rsync -a --delete --exclude modules --exclude firmware --exclude 'python3*' --exclude 'chromium*' --exclude 'wpe-webkit*' \
    --exclude 'libWPEWebKit*' --exclude 'libwebkit*' --exclude 'libjavascriptcore*' --exclude 'libLLVM*' --exclude dri \
    --exclude 'node_modules' --exclude 'go' --exclude 'jvm' --exclude dbus-daemon-launch-helper --exclude ssh \
    "$H":/usr/lib/ "$SYSROOT/usr/lib/"
  ln -sfn usr/lib "$SYSROOT/lib"
  echo "sysroot: $SYSROOT ($(du -sh "$SYSROOT" | cut -f1))" ;;
push|build)
  REV=${2:?usage: cross-push.sh push|build <rev> [<bench> | <console> --cutover]}; H=${3:-}; MODE=${4:-}
  if [ "$1" = push ]; then
    [ -n "$H" ] || { echo "usage: cross-push.sh push <rev> <bench> | push <rev> <console> --cutover" >&2; exit 2; }
    if [ "$MODE" = --cutover ]; then
      # a console: its glibc must be the sysroot's (the binaries are linked against the sysroot)
      want=$(strings "$SYSROOT/usr/lib/libc.so.6" | sed -n 's/^GNU C Library .* version \([0-9.]*\)\.$/\1/p' | head -1)
      have=$(ssh -o BatchMode=yes "$H" 'ldd --version | head -1' | grep -oE '[0-9]+\.[0-9]+$')
      [ -n "$want" ] && [ "$want" = "$have" ] || { echo "refused: $H has glibc ${have:-?}, the sysroot ${want:-?}" >&2; exit 3; }
    else
      not_van "$H"
    fi
  fi
  [ -e "$SYSROOT/usr/lib/libc.so" ] || { echo "no sysroot: run '$0 sysroot $H' first" >&2; exit 4; }
  rustup target list --installed | grep -qx $T || { echo "missing: rustup target add $T" >&2; exit 4; }
  rm -rf "$W/export" && mkdir -p "$W/export"
  git -C "$ROOT" archive "$REV" | tar -x -C "$W/export"
  git -C "$ROOT" describe --always "$REV" > "$W/export/packaging/VERSION"
  ln -s "$ROOT/ui/node_modules" "$W/export/ui/node_modules"
  (cd "$W/export/ui" && npm run build --silent)
  echo "ui bundle index $(sha256sum "$W/export/ui/dist/index.html" | cut -c1-12)"
  cflags="--target=$T --sysroot=$SYSROOT"
  ( cd "$W/export"
    export CARGO_TARGET_DIR=$W/target
    export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=clang
    export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS="-C link-arg=--target=$T -C link-arg=--sysroot=$SYSROOT -C link-arg=-fuse-ld=lld"
    export CC_aarch64_unknown_linux_gnu=clang CXX_aarch64_unknown_linux_gnu=clang++ AR_aarch64_unknown_linux_gnu=llvm-ar
    export CFLAGS_aarch64_unknown_linux_gnu="$cflags" CXXFLAGS_aarch64_unknown_linux_gnu="$cflags"
    export PKG_CONFIG_SYSROOT_DIR=$SYSROOT PKG_CONFIG_LIBDIR=$SYSROOT/usr/lib/pkgconfig:$SYSROOT/usr/share/pkgconfig PKG_CONFIG_ALLOW_CROSS=1
    cargo build --release --workspace --bins --target $T 2>&1 | tail -3 )
  B=$W/target/$T/release
  bins=$(find "$B" -maxdepth 1 -type f -name 'aetherrv-*' -perm -u+x ! -name '*.d')
  [ -n "$bins" ] || { echo "no binaries built" >&2; exit 5; }
  # shellcheck disable=SC2086  # one path per word
  file -b $bins | grep -qv 'ARM aarch64' && { echo "not all binaries are aarch64" >&2; exit 5; }
  echo "built: $(echo "$bins" | wc -l) binaries in $B"
  [ "$1" = build ] && exit 0
  # the source as push.sh syncs it, then the binaries where install.sh looks for them
  ssh "$H" "sudo mkdir -p /opt/aetherrv-rs/src"   # (rsync makes only the last level)
  rsync -a --delete --exclude target --exclude .git --exclude ui/node_modules --exclude harness/out \
    --exclude packaging/local.env --exclude '.venv' --exclude __pycache__ --exclude docs/private \
    --rsync-path="sudo rsync" "$W/export/" "$H":/opt/aetherrv-rs/src/
  ssh "$H" "sudo mkdir -p /opt/aetherrv-rs/src/target/release && sudo find /opt/aetherrv-rs/src/target/release -maxdepth 1 -name 'aetherrv-*' -delete"
  # shellcheck disable=SC2086  # one path per word
  rsync -a --rsync-path="sudo rsync" $bins "$H":/opt/aetherrv-rs/src/target/release/
  ssh "$H" "cd /opt/aetherrv-rs/src && sudo packaging/install.sh ${MODE:+--cutover}" | tail -2 ;;
*) sed -n '2,11p' "$0"; exit 2 ;;
esac
