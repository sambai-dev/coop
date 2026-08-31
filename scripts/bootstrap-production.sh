#!/usr/bin/env bash
# Bootstrap the supported privileged Compose deployment on a dedicated x86_64
# Linux VM, then verify its real API posture and one canary per runtime.
set -euo pipefail

if [[ "${ROOKHOLD_PRODUCTION_VM_ACKNOWLEDGED:-}" != true ]]; then
  echo "Rookhold's privileged Compose service is host-equivalent and must run only on a dedicated disposable x86_64 Linux VM." >&2
  echo "Set ROOKHOLD_PRODUCTION_VM_ACKNOWLEDGED=true after verifying that boundary." >&2
  exit 1
fi

test "$(uname -s)" = Linux
test "$(uname -m)" = x86_64
command -v docker >/dev/null
command -v openssl >/dev/null
command -v python3 >/dev/null
command -v curl >/dev/null
command -v sha256sum >/dev/null
docker compose version >/dev/null

reviewed_runsc_version=release-20260817.0
reviewed_runsc_sha256=048b89aada69dc3333422e139d6e9d02f8ab06bda52398060e0fbdacca00074c
runtime_dir=.coop-runtime
runsc_path=$runtime_dir/runsc
attestation_key_path=$runtime_dir/attestation-key.pem
attestation_public_key_path=$runtime_dir/attestation-public-key.pem

if [[ -L "$runtime_dir" ]]; then
  echo "$runtime_dir must not be a symlink" >&2
  exit 1
fi
install -d -m 0700 "$runtime_dir"
if [[ -L "$runsc_path" || -L "$attestation_key_path" || -L "$attestation_public_key_path" ]]; then
  echo "$runtime_dir and its trusted files must not be symlinks" >&2
  exit 1
fi

if [[ ! -e "$runsc_path" ]]; then
  temporary_runsc=$runtime_dir/runsc.download
  if [[ -e "$temporary_runsc" || -L "$temporary_runsc" ]]; then
    echo "refusing an existing temporary runsc download" >&2
    exit 1
  fi
  trap 'rm -f -- "${temporary_runsc:-}"' EXIT
  curl -fsSLo "$temporary_runsc" \
    "https://storage.googleapis.com/gvisor/releases/$reviewed_runsc_version/x86_64/runsc"
  printf '%s  %s\n' "$reviewed_runsc_sha256" "$temporary_runsc" | sha256sum -c -
  chmod 0755 "$temporary_runsc"
  mv -- "$temporary_runsc" "$runsc_path"
  temporary_runsc=
  trap - EXIT
fi
test -f "$runsc_path"
test -x "$runsc_path"
test "$(sha256sum "$runsc_path" | awk '{print $1}')" = "$reviewed_runsc_sha256"
test "$("$runsc_path" --version | head -n1)" = "runsc version $reviewed_runsc_version"

if [[ ! -e "$attestation_key_path" ]]; then
  temporary_key=$runtime_dir/attestation-key.pem.new
  if [[ -e "$temporary_key" || -L "$temporary_key" ]]; then
    echo "refusing an existing temporary attestation key" >&2
    exit 1
  fi
  umask 0077
  openssl genpkey -algorithm ED25519 -out "$temporary_key"
  chmod 0600 "$temporary_key"
  mv -- "$temporary_key" "$attestation_key_path"
fi
test -f "$attestation_key_path"
test "$(stat -c %a "$attestation_key_path")" = 600
openssl pkey -in "$attestation_key_path" -noout -check >/dev/null
if [[ ! -e "$attestation_public_key_path" ]]; then
  temporary_public_key=$runtime_dir/attestation-public-key.pem.new
  if [[ -e "$temporary_public_key" || -L "$temporary_public_key" ]]; then
    echo "refusing an existing temporary attestation public key" >&2
    exit 1
  fi
  trap 'rm -f -- "${temporary_public_key:-}"' EXIT
  openssl pkey -in "$attestation_key_path" -pubout -out "$temporary_public_key"
  chmod 0644 "$temporary_public_key"
  mv -- "$temporary_public_key" "$attestation_public_key_path"
  temporary_public_key=
  trap - EXIT
fi
test -f "$attestation_public_key_path"
test "$(stat -c %a "$attestation_public_key_path")" = 644
openssl pkey -pubin -in "$attestation_public_key_path" -noout >/dev/null
cmp <(openssl pkey -in "$attestation_key_path" -pubout) "$attestation_public_key_path"

if [[ ! -e .env ]]; then
  key=$(openssl rand -hex 32)
  umask 0077
  ROOKHOLD_BOOTSTRAP_KEY="$key" python3 - <<'PY'
import os
from pathlib import Path

source = Path(".env.example").read_text(encoding="utf-8")
value = "agent-a:" + os.environ["ROOKHOLD_BOOTSTRAP_KEY"]
rendered = source.replace("ROOKHOLD_API_KEYS=", "ROOKHOLD_API_KEYS=" + value, 1)
target = Path(".env")
target.write_text(rendered, encoding="utf-8", newline="\n")
target.chmod(0o600)
PY
else
  key=$(python3 - <<'PY'
from pathlib import Path

for raw in Path(".env").read_text(encoding="utf-8").splitlines():
    if not raw.startswith("ROOKHOLD_API_KEYS="):
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
rookhold_image=$(docker image inspect --format '{{.Id}}' "$(docker compose images -q rookhold)")
if [[ ! "$rookhold_image" =~ ^sha256:[0-9a-f]{64}$ ]]; then
  echo "Compose did not produce one immutable Rookhold image ID" >&2
  exit 1
fi

runtime_dir_absolute=$(cd -- "$runtime_dir" && pwd -P)
runsc_absolute=$runtime_dir_absolute/runsc
attestation_key_absolute=$runtime_dir_absolute/attestation-key.pem
attestation_public_key_absolute=$runtime_dir_absolute/attestation-public-key.pem

# File-backed Compose secrets and ordinary bind mounts retain host ownership.
# Stage both host inputs through the exact built image, then validate the
# root-owned copies that the service entrypoint will use. This works for a
# rootful daemon and for rootless UID mapping when the privileged provider is
# otherwise available; incompatible userns-remap layouts fail before deploy.
docker run --rm \
  --network none \
  --read-only \
  --user 0:0 \
  --cap-drop ALL \
  --cap-add DAC_OVERRIDE \
  --cap-add CHOWN \
  --security-opt no-new-privileges \
  --tmpfs /run/rookhold-runtime:rw,exec,nosuid,nodev,mode=0700,uid=0,gid=0 \
  --tmpfs /run/rookhold-secrets:rw,noexec,nosuid,nodev,mode=0700,uid=0,gid=0 \
  --mount "type=bind,src=$runsc_absolute,dst=/run/rookhold-bootstrap/runsc,readonly" \
  --mount "type=bind,src=$attestation_key_absolute,dst=/run/rookhold-bootstrap/attestation-key.pem,readonly" \
  --mount "type=bind,src=$attestation_public_key_absolute,dst=/run/rookhold-bootstrap/attestation-public-key.pem,readonly" \
  --env ROOKHOLD_GVISOR_RUNSC_SOURCE=/run/rookhold-bootstrap/runsc \
  --env ROOKHOLD_GVISOR_RUNSC=/run/rookhold-runtime/runsc \
  --env ROOKHOLD_ATTESTATION_KEY_SOURCE=/run/rookhold-bootstrap/attestation-key.pem \
  --env ROOKHOLD_ATTESTATION_KEY_FILE=/run/rookhold-secrets/attestation-key.pem \
  --env ROOKHOLD_VERIFY_PUBLIC_KEY_SOURCE=/run/rookhold-bootstrap/attestation-public-key.pem \
  --env ROOKHOLD_VERIFY_PUBLIC_KEY_FILE=/run/rookhold-secrets/attestation-public-key.pem \
  --env "ROOKHOLD_BOOTSTRAP_RUNSC_VERSION=$reviewed_runsc_version" \
  --env "ROOKHOLD_BOOTSTRAP_RUNSC_SHA256=$reviewed_runsc_sha256" \
  "$rookhold_image" /bin/sh -ec '
    test "$(stat -c %u:%g /run/rookhold-runtime/runsc)" = 0:0
    test "$(stat -c %a /run/rookhold-runtime/runsc)" = 755
    test "$(sha256sum /run/rookhold-runtime/runsc | awk '\''{print $1}'\'')" = "$ROOKHOLD_BOOTSTRAP_RUNSC_SHA256"
    test "$(/run/rookhold-runtime/runsc --version | head -n1)" = "runsc version $ROOKHOLD_BOOTSTRAP_RUNSC_VERSION"
    test "$(stat -c %u:%g /run/rookhold-secrets/attestation-key.pem)" = 0:0
    test "$(stat -c %a /run/rookhold-secrets/attestation-key.pem)" = 600
    /usr/local/bin/rookhold-verify public-key \
      --private-key /run/rookhold-secrets/attestation-key.pem \
      --output /run/rookhold-secrets/derived-public-key.pem >/dev/null
    cmp /run/rookhold-secrets/derived-public-key.pem \
      /run/rookhold-secrets/attestation-public-key.pem
  '

rootfs_digest=$(docker compose run --rm --no-deps --entrypoint /bin/sh rookhold -ec \
  "sha256sum /opt/rookhold/rootfs/.coop-rootfs.manifest | awk '{print \$1}'")
if [[ ! "$rootfs_digest" =~ ^[0-9a-f]{64}$ ]]; then
  echo "built private-rootfs manifest did not produce a SHA-256 digest" >&2
  exit 1
fi
ROOKHOLD_BOOTSTRAP_ROOTFS_DIGEST="$rootfs_digest" python3 - <<'PY'
import os
from pathlib import Path

path = Path(".env")
lines = path.read_text(encoding="utf-8").splitlines()
name = "ROOKHOLD_GVISOR_ROOTFS_SHA256"
replacement = name + "=" + os.environ["ROOKHOLD_BOOTSTRAP_ROOTFS_DIGEST"]
for index, line in enumerate(lines):
    if line.startswith(name + "="):
        lines[index] = replacement
        break
else:
    lines.append(replacement)
path.write_text("\n".join(lines) + "\n", encoding="utf-8", newline="\n")
path.chmod(0o600)
PY

# gVisor systrap needs enough virtual-memory map slots for its application
# kernel. The privileged, host-cgroup deployment makes this host setting
# visible; fail if the exact built image cannot establish the reviewed floor.
docker compose run --rm --no-deps --entrypoint /bin/sh rookhold -ec '
  current=$(cat /proc/sys/vm/max_map_count)
  if [ "$current" -lt 4194304 ]; then
    echo 4194304 > /proc/sys/vm/max_map_count
  fi
  test "$(cat /proc/sys/vm/max_map_count)" -ge 4194304
'
docker compose up --detach --wait

ROOKHOLD_CLIENT_KEY="$key" \
ROOKHOLD_VERIFY_BASE_URL="${ROOKHOLD_VERIFY_BASE_URL:-http://127.0.0.1:7300}" \
ROOKHOLD_VERIFY_MINIMUM_ISOLATION="${ROOKHOLD_VERIFY_MINIMUM_ISOLATION:-gvisor-application-kernel}" \
ROOKHOLD_VERIFY_CONTAINER_IMAGE="$rookhold_image" \
ROOKHOLD_VERIFY_PUBLIC_KEY_FILE="$attestation_public_key_absolute" \
python3 scripts/verify-production.py

echo "Rookhold is running and independently verified. Tenant credentials remain in .env; the mode-0600 signing key and pinned public key remain in .coop-runtime." >&2
