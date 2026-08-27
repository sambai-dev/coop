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

suffix="${GITHUB_RUN_ID:-local}-${GITHUB_RUN_ATTEMPT:-0}-$$"
name="coop-container-smoke-${suffix}"
key="ci-smoke-key-0123456789abcdef"

cleanup() {
  local status=$?
  trap - EXIT
  if [[ "$status" -ne 0 ]]; then
    docker logs --tail 200 "$name" >&2 2>/dev/null || true
  fi
  docker rm --force --volumes "$name" >/dev/null 2>&1 || true
  exit "$status"
}
trap cleanup EXIT

docker run --detach \
  --name "$name" \
  --privileged \
  --cgroupns=host \
  --publish 127.0.0.1::7300 \
  --env "COOP_API_KEYS=smoke:${key}" \
  --env COOP_SANDBOX=ns \
  --env COOP_SECCOMP=auto \
  --env COOP_WORKERS=1 \
  "$image" >/dev/null

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
python3 scripts/verify-production.py

docker stop --time 45 "$name" >/dev/null
