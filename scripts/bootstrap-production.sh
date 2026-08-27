#!/usr/bin/env bash
# Bootstrap the supported privileged Compose deployment on a dedicated x86_64
# Linux VM, then verify its real API posture and one canary per runtime.
set -euo pipefail

if [[ "${COOP_PRODUCTION_VM_ACKNOWLEDGED:-}" != true ]]; then
  echo "Coop's privileged Compose service is host-equivalent and must run only on a dedicated disposable x86_64 Linux VM." >&2
  echo "Set COOP_PRODUCTION_VM_ACKNOWLEDGED=true after verifying that boundary." >&2
  exit 1
fi

test "$(uname -s)" = Linux
test "$(uname -m)" = x86_64
command -v docker >/dev/null
command -v openssl >/dev/null
command -v python3 >/dev/null
docker compose version >/dev/null

if [[ ! -e .env ]]; then
  key=$(openssl rand -hex 32)
  umask 0077
  COOP_BOOTSTRAP_KEY="$key" python3 - <<'PY'
import os
from pathlib import Path

source = Path(".env.example").read_text(encoding="utf-8")
value = "agent-a:" + os.environ["COOP_BOOTSTRAP_KEY"]
rendered = source.replace("COOP_API_KEYS=", "COOP_API_KEYS=" + value, 1)
target = Path(".env")
target.write_text(rendered, encoding="utf-8", newline="\n")
target.chmod(0o600)
PY
else
  key=$(python3 - <<'PY'
from pathlib import Path

for raw in Path(".env").read_text(encoding="utf-8").splitlines():
    if not raw.startswith("COOP_API_KEYS="):
        continue
    entries = raw.split("=", 1)[1].split(",")
    for entry in entries:
        tenant, separator, key = entry.partition(":")
        if separator and tenant == "agent-a" and key:
            print(key)
            raise SystemExit(0)
raise SystemExit(".env must contain a non-empty agent-a:key entry")
PY
  )
fi

docker compose build --pull
docker compose up --detach --wait

COOP_CLIENT_KEY="$key" \
COOP_VERIFY_BASE_URL="${COOP_VERIFY_BASE_URL:-http://127.0.0.1:7300}" \
python3 scripts/verify-production.py

echo "Coop is running and verified. The agent-a key remains only in .env." >&2
