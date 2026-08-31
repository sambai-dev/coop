#!/usr/bin/env bash
# Mandatory per-job gVisor OCI lifecycle gate. This exercises the real runsc
# binary and never uses the test-only `runsc do` command.
set -euo pipefail

reviewed_version=release-20260817.0
reviewed_sha256=048b89aada69dc3333422e139d6e9d02f8ab06bda52398060e0fbdacca00074c
runsc=${ROOKHOLD_GVISOR_RUNSC:-/usr/local/bin/runsc}
rootfs=${ROOKHOLD_ROOTFS:-/opt/rookhold/rootfs}
server=${ROOKHOLD_GVISOR_SERVER_BIN:-target/debug/rookhold}
verify_bin=${ROOKHOLD_VERIFY_BIN:-"$(dirname "$server")/rookhold-verify"}
port=${ROOKHOLD_GVISOR_SMOKE_PORT:-7397}
key=rookhold-gvisor-smoke-key-with-more-than-16-characters

test "$(uname -s)" = Linux
test "$(uname -m)" = x86_64
test "$(id -u)" = 0
test -x "$runsc"
test -x "$server"
test -x "$verify_bin"
test -x "$rootfs/usr/local/bin/rookhold-oci-init"
test -f "$rootfs/.coop-rootfs.manifest"
test -r /sys/fs/cgroup/cgroup.controllers
if [[ "$(cat /proc/sys/vm/max_map_count)" -lt 4194304 ]]; then
  echo 4194304 >/proc/sys/vm/max_map_count
fi
test "$(cat /proc/sys/vm/max_map_count)" -ge 4194304
test "$(sha256sum "$runsc" | awk '{print $1}')" = "$reviewed_sha256"
"$runsc" --version | grep -Fx "runsc version $reviewed_version"

base=$(mktemp -d /var/lib/rookhold-gvisor-smoke.XXXXXX)
case "$base" in
  /var/lib/rookhold-gvisor-smoke.*) ;;
  *) echo "unsafe gVisor smoke directory: $base" >&2; exit 1 ;;
esac
install -d -o root -g root -m 0700 "$base/jobs"
"$verify_bin" generate-key --output "$base/attestation.pem"
test "$(stat -c %a "$base/attestation.pem")" = 600
rootfs_digest=$(sha256sum "$rootfs/.coop-rootfs.manifest" | awk '{print $1}')
server_pid=

stop_server() {
  if [[ -n "$server_pid" ]] && kill -0 "$server_pid" 2>/dev/null; then
    kill -TERM "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true
  fi
  server_pid=
}

cleanup() {
  status=$?
  stop_server
  if [[ "$status" -ne 0 && -f "$base/server.log" ]]; then
    cat "$base/server.log" >&2
  fi
  rm -rf -- "$base"
}
trap cleanup EXIT

start_server() {
  ROOKHOLD_ENV=production \
  ROOKHOLD_ADDR="127.0.0.1:$port" \
  ROOKHOLD_DB="$base/rookhold.db" \
  ROOKHOLD_JOBS_ROOT="$base/jobs" \
  ROOKHOLD_ROOTFS="$rootfs" \
  ROOKHOLD_SANDBOX=gvisor \
  ROOKHOLD_GVISOR_RUNSC="$runsc" \
  ROOKHOLD_GVISOR_ROOTFS_SHA256="$rootfs_digest" \
  ROOKHOLD_GVISOR_PLATFORM=systrap \
  ROOKHOLD_ATTESTATION_MODE=sign \
  ROOKHOLD_ATTESTATION_KEY_FILE="$base/attestation.pem" \
  ROOKHOLD_API_KEYS="smoke:$key" \
  RUST_LOG=info,coop_exec=debug \
    "$server" >>"$base/server.log" 2>&1 &
  server_pid=$!
}

wait_ready() {
  for _ in $(seq 1 60); do
    if READY_URL="http://127.0.0.1:$port/readyz" python3 -c \
      'import os, urllib.request; urllib.request.urlopen(os.environ["READY_URL"], timeout=2).read()' \
      >/dev/null 2>&1; then
      return 0
    fi
    if ! kill -0 "$server_pid" 2>/dev/null; then
      cat "$base/server.log" >&2
      return 1
    fi
    sleep 1
  done
  cat "$base/server.log" >&2
  return 1
}

start_server
wait_ready

BASE_URL="http://127.0.0.1:$port" ROOKHOLD_CLIENT_KEY="$key" CRASH_ID_FILE="$base/crash-id" python3 - <<'PY'
import json
import os
import time
import urllib.error
import urllib.request

base = os.environ["BASE_URL"]
headers = {
    "Authorization": f"Bearer {os.environ['ROOKHOLD_CLIENT_KEY']}",
    "Content-Type": "application/json",
}


def request(method, path, body=None, expected=(200,)):
    encoded = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(base + path, data=encoded, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=5) as response:
            assert response.status in expected, (response.status, path)
            data = response.read()
            return json.loads(data) if data else {}
    except urllib.error.HTTPError as error:
        assert error.code in expected, (error.code, error.read().decode())
        return json.loads(error.read() or b"{}")


def submit(code, *, wall=15, minimum="gvisor-application-kernel"):
    return request(
        "POST",
        "/v1/jobs",
        {
            "language": "python",
            "code": code,
            "requirements": {"minimum_isolation": minimum},
            "limits": {
                "wall_seconds": wall,
                "cpu_seconds": 10,
                "mem_mb": 256,
                "max_pids": 128,
                "max_file_mb": 16,
                "allow_network": False,
            },
        },
        (201,),
    )["job_id"]


def result(job_id, expected):
    for _ in range(60):
        value = request("GET", f"/v1/jobs/{job_id}/result?wait_seconds=1", expected=(200, 202))
        if value.get("status") in expected:
            return value
        if value.get("status") in {"succeeded", "failed", "timed_out", "oom_killed", "cancelled", "error"}:
            raise AssertionError(f"job {job_id} reached unexpected terminal state: {value}")
    raise AssertionError(f"job {job_id} did not reach {expected}")


success = submit(
    "import socket\n"
    "print('GVISOR_JOB_OK')\n"
    "try:\n"
    " socket.socket(socket.AF_INET, socket.SOCK_STREAM).connect(('1.1.1.1', 80))\n"
    " print('NETWORK_UNEXPECTED')\n"
    "except OSError:\n"
    " print('NETWORK_BLOCKED')"
)
success_result = result(success, {"succeeded"})
assert success_result["stdout"] == "GVISOR_JOB_OK\nNETWORK_BLOCKED", success_result
detail = request("GET", f"/v1/jobs/{success}")
policy = detail["execution_policy"]
receipt = detail["receipt"]
assert policy["bootstrap_ready"] is True
assert policy["isolation_class"] == "gvisor-application-kernel"
assert policy["networking"] == "disabled"
assert policy["runtime_sha256"] == "048b89aada69dc3333422e139d6e9d02f8ab06bda52398060e0fbdacca00074c"
assert len(policy["rootfs_sha256"]) == 64
assert len(policy["config_sha256"]) == 64
assert receipt["minimum_isolation"] == "gvisor-application-kernel"
assert receipt["isolation_class"] == "gvisor-application-kernel"

request(
    "POST",
    "/v1/jobs",
    {
        "language": "python",
        "code": "print('must not queue')",
        "requirements": {"minimum_isolation": "hardware-vm"},
    },
    (422,),
)

timed = submit("while True:\n pass", wall=1)
result(timed, {"timed_out"})

cancelled = submit(
    "import os, time\n"
    "if os.fork() == 0:\n"
    " while True: time.sleep(1)\n"
    "while True: time.sleep(1)"
)
for _ in range(30):
    current = request("GET", f"/v1/jobs/{cancelled}")
    if current["status"] == "running":
        break
    time.sleep(0.1)
request("DELETE", f"/v1/jobs/{cancelled}", expected=(200, 202))
result(cancelled, {"cancelled"})

crash = submit("import time\nprint('CRASH_READY', flush=True)\nwhile True: time.sleep(1)")
for _ in range(50):
    replay = request("GET", f"/v1/jobs/{crash}/replay")
    if any(event.get("kind") == "stdout" and "CRASH_READY" in json.dumps(event) for event in replay["events"]):
        break
    time.sleep(0.1)
else:
    raise AssertionError("crash-reconciliation job never became ready")
with open(os.environ["CRASH_ID_FILE"], "w", encoding="utf-8") as output:
    output.write(crash)
PY

kill -KILL "$server_pid"
wait "$server_pid" 2>/dev/null || true
server_pid=
test -n "$(find "$base/jobs" -name lease.json -print -quit)"

# Switching providers is not allowed to strand a live gVisor workload. The
# Off startup must first kill the discoverable cgroup, then fail closed because
# only the matching reviewed runtime may delete its persistent sandbox state.
if ROOKHOLD_ENV=production \
  ROOKHOLD_ADDR="127.0.0.1:$port" \
  ROOKHOLD_DB="$base/rookhold.db" \
  ROOKHOLD_JOBS_ROOT="$base/jobs" \
  ROOKHOLD_SANDBOX=off \
  ROOKHOLD_UNSAFE_ALLOW_NAIVE=true \
  ROOKHOLD_ATTESTATION_MODE=sign \
  ROOKHOLD_ATTESTATION_KEY_FILE="$base/attestation.pem" \
  ROOKHOLD_API_KEYS="smoke:$key" \
  "$server" >>"$base/server.log" 2>&1; then
  echo "Off provider unexpectedly accepted stale gVisor state" >&2
  exit 1
fi
test -n "$(find "$base/jobs" -name lease.json -print -quit)"
if find /sys/fs/cgroup -type d -name 'job-*' -path '*coop-jobs*' -print -quit | grep -q .; then
  echo "provider switch left a crashed gVisor workload cgroup" >&2
  exit 1
fi

start_server
wait_ready

BASE_URL="http://127.0.0.1:$port" ROOKHOLD_CLIENT_KEY="$key" CRASH_ID_FILE="$base/crash-id" python3 - <<'PY'
import json, os, urllib.request
job = open(os.environ["CRASH_ID_FILE"], encoding="utf-8").read().strip()
req = urllib.request.Request(
    os.environ["BASE_URL"] + f"/v1/jobs/{job}",
    headers={"Authorization": f"Bearer {os.environ['ROOKHOLD_CLIENT_KEY']}"},
)
with urllib.request.urlopen(req, timeout=5) as response:
    detail = json.load(response)
assert detail["status"] == "error", detail
PY

for _ in $(seq 1 50); do
  if [[ -z "$(find "$base/jobs" -mindepth 1 -print -quit)" ]]; then
    break
  fi
  sleep 0.1
done
test -z "$(find "$base/jobs" -mindepth 1 -print -quit)"

stop_server
if find /sys/fs/cgroup -type d -name 'job-*' -path '*coop-jobs*' -print -quit | grep -q .; then
  echo "gVisor smoke leaked a Rookhold job cgroup" >&2
  exit 1
fi

echo "gVisor OCI smoke and crash reconciliation passed"
