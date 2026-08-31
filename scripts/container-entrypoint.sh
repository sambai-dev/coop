#!/bin/sh
# Copy host-owned bootstrap inputs across the container ownership boundary
# before starting Rookhold or the packaged offline verifier.
set -eu

temporary=
cleanup() {
  if [ -n "$temporary" ]; then
    rm -f -- "$temporary"
  fi
}
trap cleanup EXIT HUP INT TERM

stage_file() {
  source_path=$1
  target_path=$2
  mode=$3
  label=$4

  if [ -L "$source_path" ] || [ ! -f "$source_path" ]; then
    echo "$label source must be a non-symlink regular file: $source_path" >&2
    exit 1
  fi
  if [ "$(stat -c %a "$source_path")" != "$mode" ]; then
    echo "$label source must have mode $mode: $source_path" >&2
    exit 1
  fi
  case "$target_path" in
    /run/rookhold-runtime/*)
      target_name=${target_path#/run/rookhold-runtime/}
      ;;
    /run/rookhold-secrets/*)
      target_name=${target_path#/run/rookhold-secrets/}
      ;;
    *)
      echo "$label target must be below /run/rookhold-runtime or /run/rookhold-secrets" >&2
      exit 1
      ;;
  esac
  case "$target_name" in
    "" | . | .. | */*)
      echo "$label target must be a direct child of its trusted runtime directory" >&2
      exit 1
      ;;
  esac

  target_parent=${target_path%/*}
  if [ -L "$target_parent" ]; then
    echo "$label target directory must not be a symlink" >&2
    exit 1
  fi
  install -d -o 0 -g 0 -m 0700 "$target_parent"
  temporary="$target_path.new.$$"
  if [ -e "$temporary" ] || [ -L "$temporary" ]; then
    echo "$label temporary target already exists" >&2
    exit 1
  fi
  install -o 0 -g 0 -m "$mode" "$source_path" "$temporary"
  mv -- "$temporary" "$target_path"
  temporary=
  if [ "$(stat -c %u:%g "$target_path")" != 0:0 ] ||
     [ "$(stat -c %a "$target_path")" != "$mode" ]; then
    echo "$label staging did not produce the required root ownership and mode" >&2
    exit 1
  fi
}

if [ -n "${ROOKHOLD_GVISOR_RUNSC_SOURCE:-}" ]; then
  stage_file \
    "$ROOKHOLD_GVISOR_RUNSC_SOURCE" \
    "${ROOKHOLD_GVISOR_RUNSC:-/run/rookhold-runtime/runsc}" \
    755 \
    "gVisor runtime"
fi

if [ -n "${ROOKHOLD_ATTESTATION_KEY_SOURCE:-}" ]; then
  stage_file \
    "$ROOKHOLD_ATTESTATION_KEY_SOURCE" \
    "${ROOKHOLD_ATTESTATION_KEY_FILE:-/run/rookhold-secrets/attestation-key.pem}" \
    600 \
    "attestation private key"
fi

if [ -n "${ROOKHOLD_VERIFY_PUBLIC_KEY_SOURCE:-}" ]; then
  stage_file \
    "$ROOKHOLD_VERIFY_PUBLIC_KEY_SOURCE" \
    "${ROOKHOLD_VERIFY_PUBLIC_KEY_FILE:-/run/rookhold-secrets/trusted-attestation-public-key.pem}" \
    644 \
    "trusted attestation public key"
fi

trap - EXIT HUP INT TERM
if [ "$#" -eq 0 ]; then
  echo "container entrypoint requires a command" >&2
  exit 1
fi
case "$1" in
  -*) set -- /usr/local/bin/rookhold "$@" ;;
esac
exec "$@"
