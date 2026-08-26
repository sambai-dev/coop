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
  sleep 1
done
test "$ready" = true

status_json=$(curl --silent --show-error --fail --max-time 5 --max-filesize 1048576 \
  --header "Authorization: Bearer ${key}" \
  "$base/v1/status")
printf '%s' "$status_json" | python3 -c '
import json, sys
status = json.load(sys.stdin)
execution = status["execution"]
assert execution["backend"] == "namespaces+cgroups-v2+private-rootfs", status
assert execution["isolated"] is True, status
assert execution["private_rootfs"] is True, status
assert execution["dedicated_bootstrap"] is True, status
assert execution["seccomp"] is True, status
assert execution["networking"] == "disabled", status
assert status["storage_ready"] is True, status
'

capabilities_json=$(curl --silent --show-error --fail --max-time 5 --max-filesize 1048576 \
  --header "Authorization: Bearer ${key}" \
  "$base/v1/capabilities")
printf '%s' "$capabilities_json" | python3 -c '
import json, sys
capabilities = json.load(sys.stdin)
assert set(capabilities["languages"]) == {"python", "node", "bash"}, capabilities
assert capabilities["execution"]["networking"] == "disabled", capabilities
assert capabilities["features"]["receipts"] is True, capabilities
'

smoke_job() {
  local language="$1"
  local code="$2"
  local expected="$3"
  local payload submit_json job_id result_json detail_json

  payload=$(LANGUAGE="$language" CODE="$code" python3 -c '
import json, os
print(json.dumps({
    "language": os.environ["LANGUAGE"],
    "code": os.environ["CODE"],
    "limits": {"wall_seconds": 10, "mem_mb": 128, "allow_network": False},
}, separators=(",", ":")))
')
  submit_json=$(curl --silent --show-error --fail --max-time 10 --max-filesize 1048576 \
    --request POST \
    --header "Authorization: Bearer ${key}" \
    --header 'Content-Type: application/json' \
    --data-binary "$payload" \
    "$base/v1/jobs")
  job_id=$(printf '%s' "$submit_json" | python3 -c 'import json,sys; print(json.load(sys.stdin)["job_id"])')
  test -n "$job_id"

  result_json=$(curl --silent --show-error --fail --max-time 70 --max-filesize 4194304 \
    --header "Authorization: Bearer ${key}" \
    "$base/v1/jobs/${job_id}/result?wait_seconds=60")
  printf '%s' "$result_json" | python3 -c '
import json, sys
result = json.load(sys.stdin)
expected = sys.argv[1]
assert result["status"] == "succeeded", result
assert result["exit_code"] == 0, result
assert result["stdout"] == expected, result
assert result["stderr"] == "", result
assert result["truncated"] is False, result
' "$expected"

  detail_json=$(curl --silent --show-error --fail --max-time 5 --max-filesize 1048576 \
    --header "Authorization: Bearer ${key}" \
    "$base/v1/jobs/${job_id}")
  printf '%s' "$detail_json" | python3 -c '
import hashlib, json, sys
detail = json.load(sys.stdin)
assert detail["status"] == "succeeded", detail
assert detail["effective_spec"]["limits"]["allow_network"] is False, detail
policy = detail["execution_policy"]
assert policy["sandbox"] == "namespaces+cgroups-v2+private-rootfs", detail
assert policy["seccomp"] is True, detail
assert policy["network_allowed"] is False, detail
assert policy["networking"] == "disabled", detail
assert policy["private_rootfs"] is True, detail
assert policy["dedicated_bootstrap"] is True, detail
receipt = detail["receipt"]
assert receipt["backend"] == "namespaces+cgroups-v2+private-rootfs", receipt
assert receipt["seccomp"] is True, receipt
assert receipt["network_allowed"] is False, receipt
assert receipt["networking"] == "disabled", receipt
assert receipt["evidence_complete"] is True, receipt
assert receipt["event_chain"]["complete"] is True, receipt
assert receipt["event_chain"]["events"] > 0, receipt
assert len(receipt["event_chain"]["head"]) == 64, receipt
assert receipt["output"]["truncated"] is False, receipt
assert len(receipt["output"]["stdout_sha256"]) == 64, receipt
assert len(receipt["output"]["stderr_sha256"]) == 64, receipt
recorded = receipt["receipt_sha256"]
assert recorded == detail["receipt_sha256"], detail
unsigned = dict(receipt)
unsigned.pop("receipt_sha256")
canonical = json.dumps(unsigned, ensure_ascii=False, sort_keys=True, separators=(",", ":"))
assert hashlib.sha256(canonical.encode()).hexdigest() == recorded, receipt
'
}

smoke_job python 'print("container-smoke-python")' container-smoke-python
smoke_job node 'console.log("container-smoke-node")' container-smoke-node
smoke_job bash 'printf "%s\n" "container-smoke-bash"' container-smoke-bash

docker stop --time 45 "$name" >/dev/null
