#!/usr/bin/env bash
# Construct the private rootfs used by privileged hostile tests inside the
# pinned Debian CI container. This intentionally excludes /usr/local, where
# the Rust toolchain and CI credentials/caches live.
set -euo pipefail

if [[ "$(id -u)" != "0" ]]; then
  echo "prepare-test-rootfs must run as root" >&2
  exit 1
fi

rootfs="${COOP_ROOTFS:-/opt/coop/rootfs}"
helper_source="${1:-target/debug/coop-sandbox-init}"
helper_target="${COOP_SANDBOX_HELPER:-/usr/local/bin/coop-sandbox-init}"

if [[ "$rootfs" != "/opt/coop/rootfs" ]]; then
  echo "refusing unexpected test rootfs path: $rootfs" >&2
  exit 1
fi
if [[ -e "$rootfs" ]]; then
  echo "refusing to reuse existing test rootfs: $rootfs" >&2
  exit 1
fi
if [[ ! -x "$helper_source" ]]; then
  echo "sandbox helper was not built: $helper_source" >&2
  exit 1
fi

install -o root -g root -m 0755 "$helper_source" "$helper_target"
install -d -o root -g root -m 0755 "$rootfs" "$rootfs/usr" "$rootfs/etc"

# Debian's merged-/usr links preserve the interpreter paths used outside and
# inside the pivot. Copy distribution runtimes/libraries, but not /usr/local.
for link in bin lib lib64 sbin; do
  if [[ -e "/$link" || -L "/$link" ]]; then
    cp -a "/$link" "$rootfs/$link"
  fi
done
for tree in bin sbin lib lib64 share; do
  if [[ -e "/usr/$tree" ]]; then
    cp -a "/usr/$tree" "$rootfs/usr/$tree"
  fi
done

for config in \
  group \
  host.conf \
  ld.so.cache \
  ld.so.conf \
  ld.so.conf.d \
  nsswitch.conf \
  passwd; do
  if [[ -e "/etc/$config" ]]; then
    cp -a "/etc/$config" "$rootfs/etc/$config"
  fi
done
install -d -o root -g root -m 0755 "$rootfs/etc/ssl"
if [[ -d /etc/ssl/certs ]]; then
  cp -a /etc/ssl/certs "$rootfs/etc/ssl/certs"
fi
if [[ -f /etc/ssl/openssl.cnf ]]; then
  cp -a /etc/ssl/openssl.cnf "$rootfs/etc/ssl/openssl.cnf"
fi

install -d -o root -g root -m 0755 \
  "$rootfs/.pivot_old" \
  "$rootfs/proc" \
  "$rootfs/dev" \
  "$rootfs/work" \
  "$rootfs/var"
install -d -o root -g root -m 1777 "$rootfs/tmp" "$rootfs/var/tmp"
install -d -o root -g root -m 0755 "$rootfs/tmp/home"

test -x "$rootfs/usr/bin/python3"
test -x "$rootfs/usr/bin/node"
test -x "$rootfs/usr/bin/bash"
test -z "$(find "$rootfs/.pivot_old" -mindepth 1 -print -quit)"
