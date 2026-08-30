#!/usr/bin/env bash
# Exercise the final image through its public API and packaged namespace
# boundary. The caller must provide an x86_64 Linux Docker host on a dedicated
# CI runner or VM; this intentionally uses the same host-equivalent privileges
# documented for Compose.
set -euo pipefail

image="${1:?usage: smoke-container.sh IMAGE}"
if [[ ! "$image" =~ ^[A-Za-z0-9][A-Za-z0-9._/@:-]*$ ]]; then
  echo "refusing invalid Docker image reference: $image" >&2
  exit 1
fi
if [[ "${CI:-}" != true && "${COOP_SMOKE_ALLOW_PRIVILEGED:-}" != true ]]; then
  echo "this smoke runs a host-equivalent privileged container; use only an ephemeral CI runner or dedicated VM" >&2
  echo "set COOP_SMOKE_ALLOW_PRIVILEGED=true to acknowledge a deliberate local run" >&2
  exit 1
fi
test "$(uname -m)" = x86_64
command -v docker >/dev/null
command -v curl >/dev/null
command -v python3 >/dev/null
command -v seq >/dev/null
image_id=$(docker image inspect --format '{{.Id}}' "$image")
if [[ ! "$image_id" =~ ^sha256:[0-9a-f]{64}$ ]]; then
  echo "Docker did not resolve an immutable image ID for $image" >&2
  exit 1
fi

suffix="${GITHUB_RUN_ID:-local}-${GITHUB_RUN_ATTEMPT:-0}-$$"
name="coop-container-smoke-${suffix}"
key="ci-smoke-key-0123456789abcdef"
secrets_dir=$(mktemp -d "${TMPDIR:-/tmp}/coop-container-smoke.XXXXXX")
case "$secrets_dir" in
  "${TMPDIR:-/tmp}"/coop-container-smoke.*) ;;
  *) echo "unsafe smoke secret directory: $secrets_dir" >&2; exit 1 ;;
esac
umask 0077
if [[ "$secrets_dir" == *,* ]]; then
  echo "smoke secret directory must not contain commas" >&2
  exit 1
fi
host_uid=$(id -u)
host_gid=$(id -g)
docker run --rm \
  --network none \
  --read-only \
  --user "$host_uid:$host_gid" \
  --cap-drop ALL \
  --security-opt no-new-privileges \
  --tmpfs "/run/coop-secrets:rw,noexec,nosuid,nodev,mode=0700,uid=$host_uid,gid=$host_gid" \
  --mount "type=bind,src=$secrets_dir,dst=/keys" \
  --entrypoint /bin/sh \
  "$image_id" -ec '
    /usr/local/bin/coop-verify generate-key \
      --output /run/coop-secrets/attestation.pem >/dev/null
    /usr/local/bin/coop-verify public-key \
      --private-key /run/coop-secrets/attestation.pem \
      --output /run/coop-secrets/attestation-public-key.pem >/dev/null
    install -m 0600 /run/coop-secrets/attestation.pem /keys/attestation.pem
    install -m 0644 /run/coop-secrets/attestation-public-key.pem \
      /keys/attestation-public-key.pem
  '
test "$(stat -c %a "$secrets_dir/attestation.pem")" = 600
test "$(stat -c %a "$secrets_dir/attestation-public-key.pem")" = 644

cleanup() {
  local status=$?
  trap - EXIT
  if [[ "$status" -ne 0 ]]; then
    docker logs --tail 200 "$name" >&2 2>/dev/null || true
  fi
  docker rm --force --volumes "$name" >/dev/null 2>&1 || true
  rm -rf -- "$secrets_dir"
  exit "$status"
}
trap cleanup EXIT

docker run --detach \
  --name "$name" \
  --privileged \
  --cgroupns=host \
  --publish 127.0.0.1::7300 \
  --env "COOP_API_KEYS=smoke:${key}" \
  --env COOP_ATTESTATION_MODE=sign \
  --env COOP_ATTESTATION_KEY_SOURCE=/run/coop-bootstrap/attestation-key.pem \
  --env COOP_ATTESTATION_KEY_FILE=/run/coop-secrets/attestation-key.pem \
  --env COOP_SANDBOX=ns \
  --env COOP_SECCOMP=auto \
  --env COOP_WORKERS=1 \
  --tmpfs /run/coop-secrets:rw,noexec,nosuid,nodev,mode=0700,uid=0,gid=0 \
  --volume "$secrets_dir/attestation.pem:/run/coop-bootstrap/attestation-key.pem:ro" \
  "$image_id" >/dev/null

mapping=$(docker port "$name" 7300/tcp)
port="${mapping##*:}"
base="http://127.0.0.1:${port}"

ready=false
for _ in $(seq 1 60); do
  if curl --silent --fail --max-time 2 "$base/readyz" >/dev/null 2>&1; then
    ready=true
    break
  fi
  # Startup preflight failures terminate the service. Surface their logs now
  # instead of idling through the complete readiness allowance.
  if ! docker inspect --format '{{.State.Running}}' "$name" | grep -qx true; then
    break
  fi
  sleep 1
done
test "$ready" = true

COOP_CLIENT_KEY="$key" \
COOP_VERIFY_BASE_URL="$base" \
COOP_VERIFY_LANGUAGES=python,node,bash \
COOP_VERIFY_MINIMUM_ISOLATION=linux-shared-kernel \
COOP_VERIFY_CONTAINER_IMAGE="$image_id" \
COOP_VERIFY_PUBLIC_KEY_FILE="$secrets_dir/attestation-public-key.pem" \
python3 scripts/verify-production.py

COOP_CLIENT_KEY="$key" \
COOP_VERIFY_BASE_URL="$base" \
COOP_VERIFY_MINIMUM_ISOLATION=linux-shared-kernel \
PYTHONPATH=sdks/python \
python3 scripts/verify-python-adapter.py

docker stop --time 45 "$name" >/dev/null
