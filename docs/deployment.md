# Deployment

Coop v0.5 retains three explicit operating postures: per-job gVisor OCI on a dedicated Linux x86_64 VM (the guarded production default), the weaker Linux x86_64 namespace fallback, and an unisolated same-trust development subprocess. No mode silently satisfies a stronger requested isolation class.

## Prerequisites for isolated providers

- an x86_64 Linux host; isolated providers do not support other architectures
- Linux 5.14 or newer with a unified, writable cgroup v2 hierarchy (`cgroup.kill` and recursive `mount_setattr` are required)
- effective UID 0; the current backend does not support non-root delegation
- a dedicated VM with no unrelated workloads
- a trusted private rootfs containing the configured interpreters
- for gVisor: the exact reviewed `runsc`, `vm.max_map_count >= 4194304`, the rootfs manifest digest, and the matching `coop-oci-init`
- a dedicated SQLite path outside that rootfs; the database must be a regular,
  non-symlinked file with exactly one hard link
- scoped credentials (or migration-only high-entropy tenant keys), an owner-only Ed25519 signing key, and TLS/private-network ingress

Validate the exact deployment by running the hostile suite. “The process starts” and “the health check is green” are not containment tests.

## Compose on a dedicated VM

On an x86_64 Linux Docker host, the repository image constructs `/opt/coop/rootfs` separately from the outer container filesystem. Base images are digest-pinned and both outer runtime and job rootfs install interpreters from the same dated Debian snapshot. Compose defaults to `COOP_SANDBOX=gvisor`, bind-mounts the reviewed runtime read-only, mounts the signing key as a file-backed secret, binds the exact manifest digest, and fails without credentials. Because bind mounts retain host ownership, the image entrypoint copies the runtime and private key into separate root-owned container-local tmpfs paths before it execs Coop. The strict key/runtime readers validate those staged copies. The Dockerfile refuses another production architecture. Coop runs as container PID 1 with host-cgroup delegation; do not add Docker's `init: true` or unrelated co-processes.

Use the guarded bootstrap. It refuses non-Linux/non-x86_64 hosts, requires an
explicit dedicated-VM acknowledgement, creates `.env` plus `.coop-runtime/`
without overwriting existing keys, downloads and verifies the pinned `runsc`,
generates an Ed25519 key plus a locally derived public-key pin, builds with
pulled bases, validates the root-owned staged key/runtime through that exact
image, computes the exact rootfs digest, establishes the gVisor map-count
floor, waits for readiness, and runs one receipt-and-attestation-checked canary
in each runtime:

```bash
COOP_PRODUCTION_VM_ACKNOWLEDGED=true scripts/bootstrap-production.sh
```

The guard is deliberate: `.env` contains bearer credentials and
`.coop-runtime/attestation-key.pem` is signing authority. The adjacent
`attestation-public-key.pem` is the bootstrap's operator-side trust pin; copy
it through an authenticated channel before relying on a remote deployment. If
`.env` exists, the script requires a non-empty `agent-a:key`; existing
runtime/key/pin files must match the reviewed contract. Review both scripts
before running them.

Confirm posture with an authenticated request:

```bash
curl --fail-with-body \
  -H "Authorization: Bearer $key" \
  http://127.0.0.1:7300/v1/status
```

To repeat the full posture and canary gate without rebuilding, extract the
tenant key through your secret manager, select the exact built image, and pass
the independently retained public-key pin explicitly:

```bash
coop_image=$(docker image inspect --format '{{.Id}}' "$(docker compose images -q coop)")
COOP_CLIENT_KEY="$key" \
COOP_VERIFY_CONTAINER_IMAGE="$coop_image" \
COOP_VERIFY_PUBLIC_KEY_FILE="$(pwd -P)/.coop-runtime/attestation-public-key.pem" \
COOP_VERIFY_MINIMUM_ISOLATION=gvisor-application-kernel \
  python3 scripts/verify-production.py
```

For a binary/systemd installation, set `COOP_VERIFY_BIN` to the packaged
absolute `coop-verify` path instead of `COOP_VERIFY_CONTAINER_IMAGE`. The pin
is mandatory in both modes. The verifier never obtains trust from
`/v1/attestation/public-key`.

The verifier permits plain HTTP only on loopback. Set `COOP_VERIFY_BASE_URL` to
an HTTPS endpoint for remote ingress and `COOP_VERIFY_LANGUAGES` to a
comma-separated subset when the deployment intentionally disables a runtime.
`COOP_VERIFY_MINIMUM_ISOLATION` accepts the API's exact isolation-class values;
the script default remains `linux-shared-kernel` for generic source use, while
the guarded bootstrap passes `gvisor-application-kernel`. A shared-kernel
minimum also accepts gVisor,
hardware-VM, and confidential-VM providers. Set it to
`gvisor-application-kernel`, `hardware-vm`, or `confidential-vm` when the
deployment contract requires that stronger class specifically. Wasm is a
separate capability branch and does not satisfy a Linux/VM minimum.

To exercise the bundled Python SDK and MCP adapter against the same live
server—including keyed replay, typed cancellation, atomic minimum isolation,
and terminal isolation evidence—run:

```bash
PYTHONPATH=sdks/python \
COOP_CLIENT_KEY="$key" \
COOP_VERIFY_BASE_URL=http://127.0.0.1:7300 \
COOP_VERIFY_MINIMUM_ISOLATION=gvisor-application-kernel \
  python3 scripts/verify-python-adapter.py
```

Before admitting jobs, compare `execution.isolation_class` with the configured
minimum using the API's satisfaction order, require disabled networking and all
five limit-enforcement flags for isolated process providers, require
`storage_ready: true`, and require `scheduler.shutting_down: false`. Namespace
deployments must additionally report the private rootfs, dedicated bootstrap,
and Coop seccomp filter. gVisor deliberately reports `seccomp: false` for the
namespace-guest filter; its terminal policy and receipt must instead carry the
reviewed runtime, rootfs, and OCI-config SHA-256 digests. The verifier submits
the minimum atomically with every canary; confirms requested, effective,
policy, and receipt isolation records agree; then waits for each signed
attestation, downloads exact envelope/result bytes, checks every declared
digest, length, media type, receipt binding, and result field, and invokes the
packaged `coop-verify` against those exact bytes with the explicit pin.

Rootless Docker and rootful daemons with user-namespace remapping are not a
supported containment posture: the current providers require effective UID 0,
host cgroups, mounts, and sysctl access. The staging copy follows the daemon's
UID mapping where possible and the bootstrap validates the result before
deployment; an incompatible mapping fails closed rather than relaxing file
ownership checks.

The Compose service remains privileged because the outer control plane creates runsc workloads, mounts, and cgroups. A privileged-container compromise is effectively a host compromise even though tenant jobs cross gVisor. Run this only inside the dedicated VM; never add the Docker socket, cloud credentials, home directories, or unrelated volumes.

The service publishes only `127.0.0.1:7300`. To serve remote clients, place a TLS proxy on the VM and limit its client network. Preserve `Authorization` and WebSocket upgrade headers, disable request-body logging, and avoid logging query strings.

## Binary/systemd installation and private rootfs

Source deployments must build and patch their own rootfs. On Debian-family hosts, `debootstrap` provides a functional starting point; use an approved snapshot mirror rather than the rolling mirror below when reproducibility is required:

Build and install the control-plane, verifier, and execution helpers from one checkout:

```bash
cargo build --locked --release -p coop-server -p coop-exec -p coop-attestation --bins
sudo install -o root -g root -m 0755 target/release/coop /usr/local/bin/coop
sudo install -o root -g root -m 0755 \
  target/release/coop-sandbox-init /usr/local/bin/coop-sandbox-init
sudo install -o root -g root -m 0755 \
  target/release/coop-verify /usr/local/bin/coop-verify
```

```bash
sudo debootstrap \
  --variant=minbase \
  --include=python3,nodejs,bash,ca-certificates \
  bookworm /opt/coop/rootfs https://deb.debian.org/debian
sudo install -d -m 0755 \
  /opt/coop/rootfs/.pivot_old \
  /opt/coop/rootfs/proc \
  /opt/coop/rootfs/dev \
  /opt/coop/rootfs/work
sudo install -d -m 1777 /opt/coop/rootfs/tmp
sudo install -d -m 0755 /opt/coop/rootfs/tmp/home
sudo chown -R root:root /opt/coop/rootfs
sudo chmod -R go-w /opt/coop/rootfs
sudo install -o root -g root -m 0755 \
  target/release/coop-oci-init /opt/coop/rootfs/usr/local/bin/coop-oci-init
sudo python3 scripts/build-rootfs-manifest.py /opt/coop/rootfs
```

For reproducible deployments, replace the rolling mirror with an approved snapshot and record the package manifest and tree digest. Create empty `/.pivot_old`, `/proc`, `/dev`, `/tmp/home`, and `/work` mount points if the rootfs tool did not. Keep `/.pivot_old` empty. Do not copy the host root or mount the Coop data directory into the rootfs.

Set at minimum:

```bash
export COOP_ENV=production
export COOP_SANDBOX=gvisor
export COOP_ROOTFS=/opt/coop/rootfs
export COOP_SANDBOX_HELPER=/usr/local/bin/coop-sandbox-init
export COOP_GVISOR_RUNSC=/usr/local/bin/runsc
export COOP_GVISOR_ROOTFS_SHA256="$(sha256sum /opt/coop/rootfs/.coop-rootfs.manifest | awk '{print $1}')"
export COOP_GVISOR_PLATFORM=systrap
export COOP_ATTESTATION_MODE=sign
export COOP_ATTESTATION_KEY_FILE=/etc/coop/attestation-key.pem
export COOP_JOBS_ROOT=/var/lib/coop/jobs
export COOP_DB=/var/lib/coop/coop.db
export COOP_API_KEYS="agent-a:$(openssl rand -hex 32)"
```

Provision `/usr/local/bin/runsc` from the exact version and SHA-256 pinned in
`scripts/smoke-gvisor.sh`; do not use an unreviewed distribution package or
moving URL. Generate the signing key once with
`sudo coop-verify generate-key --output /etc/coop/attestation-key.pem`, keep it
mode `0600`, distribute the derived public key through a separate trusted
channel, and retain previous public keys across rotation. The namespace
fallback uses `COOP_SANDBOX=ns` and the matching helper, but its advertised
minimum is only `linux-shared-kernel`.

Interpreter overrides name executable paths inside the private rootfs; an absolute override such as `/usr/bin/python3` must resolve beneath that root. Test Python, Node, and Bash canaries after every rootfs update.

Install `coop`, `coop-verify`, `coop-sandbox-init`, and `coop-oci-init` from the
same build. The namespace helper performs rootfs/PID/credential/seccomp setup;
the OCI init proves the process is inside gVisor before launching user code.
Do not substitute binaries from another release or make them writable by job
credentials.

`deploy/coop.service`, `deploy/coop.env.example`, and `deploy/Caddyfile.example` are starting templates for systemd and TLS ingress. The Caddy template caps request headers at 32 KiB, bounds header/body reads at 30 seconds, allows six minutes for the maximum five-minute `/result` wait plus transfer margin, and expires idle connections after 10 minutes. The unit rejects non-x86_64 hosts and creates `/var/lib/coop` with mode `0700`. Install the environment file as root with mode `0600`, fill its blank key, review paths/capabilities against your distribution, run `systemd-analyze security coop.service`, and execute the hostile suite before admitting traffic. `Delegate=yes` and writable cgroup v2 delegation are required.

## Local development

On macOS, Windows, non-x86_64 Linux, or x86_64 Linux without namespace prerequisites, development mode can use the subprocess backend:

```bash
COOP_SANDBOX=off \
COOP_JOBS_ROOT="$PWD/.coop-dev/jobs" \
cargo run --locked -p coop-server
```

This mode is not isolated: submitted code has the service account's filesystem and network access. It enforces wall time, cancellation, and bounded output, but not requested CPU, memory, process-count, or file-size controls. At startup it canaries Python, Node.js, and Bash under the job environment; only passing runtimes appear in `/v1/capabilities`, and their exact executable paths are reused for jobs. Keep the listener on loopback and submit only code you trust. Production mode requires the conspicuous `COOP_UNSAFE_ALLOW_NAIVE=true` acknowledgement for an explicit `off` setting; that acknowledgement does not make the mode safer.

On PowerShell, set `$env:COOP_SANDBOX = "off"` and `$env:COOP_JOBS_ROOT = Join-Path (Get-Location) ".coop-dev\jobs"` before `cargo run --locked -p coop-server`.

## Prebuilt archives

Before using a moving `releases/latest` URL, verify that it resolves to v0.5.0 or newer. Older release lines are unsupported for new deployments. Release archives are named by Rust target and include documentation, deploy templates, integration templates, SDK source, and `coop-verify`. The Linux archive also includes both execution init helpers.

The following commands require a current [GitHub CLI](https://cli.github.com/) with artifact-attestation support; authenticate it according to your organization's policy before downloading. For Linux x86_64:

```bash
set -euo pipefail
version=0.5.0
asset=coop-x86_64-unknown-linux-musl.tar.gz
gh release download "v${version}" --repo sambai-dev/coop \
  --pattern "$asset" --pattern SHA256SUMS
verify_github_asset() {
  gh release verify-asset "v${version}" "$1" --repo sambai-dev/coop
  gh attestation verify "$1" \
    --repo sambai-dev/coop \
    --signer-workflow sambai-dev/coop/.github/workflows/release.yml \
    --source-ref "refs/tags/v${version}" \
    --predicate-type https://slsa.dev/provenance/v1 \
    --deny-self-hosted-runners
}
verify_github_asset SHA256SUMS
expected=$(awk -v file="$asset" '
  $2 == file && $1 ~ /^[0-9a-f]{64}$/ { digest=$1; count++ }
  END { if (count != 1) exit 1; print digest }
' SHA256SUMS)
printf '%s  %s\n' "$expected" "$asset" | sha256sum --check --strict -
verify_github_asset "$asset"
tar -xzf "$asset"
sudo install -o root -g root -m 0755 \
  coop-x86_64-unknown-linux-musl/coop /usr/local/bin/coop
sudo install -o root -g root -m 0755 \
  coop-x86_64-unknown-linux-musl/coop-sandbox-init \
  /usr/local/bin/coop-sandbox-init
sudo install -o root -g root -m 0755 \
  coop-x86_64-unknown-linux-musl/coop-oci-init \
  /usr/local/bin/coop-oci-init
sudo install -o root -g root -m 0755 \
  coop-x86_64-unknown-linux-musl/coop-verify \
  /usr/local/bin/coop-verify
```

This installs binaries only. Build the private rootfs, configuration, service, and TLS ingress described above before starting production.

For an Apple-silicon macOS development installation:

```bash
set -euo pipefail
version=0.5.0
asset=coop-aarch64-apple-darwin.tar.gz
gh release download "v${version}" --repo sambai-dev/coop \
  --pattern "$asset" --pattern SHA256SUMS
verify_github_asset() {
  gh release verify-asset "v${version}" "$1" --repo sambai-dev/coop
  gh attestation verify "$1" \
    --repo sambai-dev/coop \
    --signer-workflow sambai-dev/coop/.github/workflows/release.yml \
    --source-ref "refs/tags/v${version}" \
    --predicate-type https://slsa.dev/provenance/v1 \
    --deny-self-hosted-runners
}
verify_github_asset SHA256SUMS
expected=$(awk -v file="$asset" '
  $2 == file && $1 ~ /^[0-9a-f]{64}$/ { digest=$1; count++ }
  END { if (count != 1) exit 1; print digest }
' SHA256SUMS)
actual=$(shasum -a 256 "$asset" | awk '{ print $1 }')
test "$actual" = "$expected"
verify_github_asset "$asset"
tar -xzf "$asset"
install -d "$HOME/.local/bin"
install -m 0755 coop-aarch64-apple-darwin/coop "$HOME/.local/bin/coop"
install -m 0755 coop-aarch64-apple-darwin/coop-verify "$HOME/.local/bin/coop-verify"
```

For an x86_64 Windows development installation in PowerShell:

```powershell
$ErrorActionPreference = "Stop"
$version = "0.5.0"
$asset = "coop-x86_64-pc-windows-msvc.zip"
gh release download "v$version" --repo sambai-dev/coop `
    --pattern $asset --pattern SHA256SUMS
if ($LASTEXITCODE -ne 0) { throw "release download failed" }
gh release verify-asset "v$version" SHA256SUMS --repo sambai-dev/coop
if ($LASTEXITCODE -ne 0) { throw "release verification failed for SHA256SUMS" }
gh attestation verify SHA256SUMS `
    --repo sambai-dev/coop `
    --signer-workflow sambai-dev/coop/.github/workflows/release.yml `
    --source-ref "refs/tags/v$version" `
    --predicate-type https://slsa.dev/provenance/v1 `
    --deny-self-hosted-runners
if ($LASTEXITCODE -ne 0) { throw "workflow provenance failed for SHA256SUMS" }
$escapedAsset = [regex]::Escape($asset)
$lines = @(Get-Content -LiteralPath SHA256SUMS | Where-Object {
    $_ -match "^([0-9a-f]{64})  $escapedAsset$"
})
if ($lines.Count -ne 1) { throw "checksum must contain exactly one row for $asset" }
$line = $lines[0]
$expected = ($line -split '\s+')[0].ToLowerInvariant()
$actual = (Get-FileHash -Algorithm SHA256 $asset).Hash.ToLowerInvariant()
if ($actual -ne $expected) { throw "checksum mismatch: $asset" }
gh release verify-asset "v$version" $asset --repo sambai-dev/coop
if ($LASTEXITCODE -ne 0) { throw "release verification failed for $asset" }
gh attestation verify $asset `
    --repo sambai-dev/coop `
    --signer-workflow sambai-dev/coop/.github/workflows/release.yml `
    --source-ref "refs/tags/v$version" `
    --predicate-type https://slsa.dev/provenance/v1 `
    --deny-self-hosted-runners
if ($LASTEXITCODE -ne 0) { throw "workflow provenance failed for $asset" }
Expand-Archive -Path $asset -DestinationPath . -Force
New-Item -ItemType Directory -Force "$HOME\bin" | Out-Null
Copy-Item "coop-x86_64-pc-windows-msvc\coop.exe" "$HOME\bin\coop.exe"
Copy-Item "coop-x86_64-pc-windows-msvc\coop-verify.exe" "$HOME\bin\coop-verify.exe"
```

Add the chosen user-local binary directory to `PATH`. The checksum file and combined SPDX JSON SBOM are release assets. `SHA256SUMS` names and hashes the other seven assets; its own release and workflow attestations authenticate the downloaded manifest. The SPDX inventories freshly extracted content from the six built archives/packages, and its separate SBOM attestation binds that document to those payload digests. The constrained provenance check authenticates the expected release workflow and tag for the exact bytes; it does not turn a development subprocess into isolation or gVisor into trusted hardware.

The Apple-silicon macOS and x86_64 Windows archives run the local-development subprocess backend only. Non-x86_64 Linux source builds have the same limitation. They are useful for trusted-code integration work, not production containment. A production x86_64 Linux binary installation still needs the private rootfs, cgroup/systemd setup, keys, TLS ingress, and hostile-suite validation described above.

## TLS proxy requirements

- TLS 1.2 or newer, with certificate validation by clients
- a request-header deadline no longer than 30 seconds, a bounded header size (the Caddy template uses 32 KiB), and body/connection limits compatible with Coop's API limits
- WebSocket upgrade support and long enough idle timeout for the maximum job wall time
- a response-write budget longer than the maximum 300-second `/result` wait (the Caddy template uses six minutes)
- no bearer-token, body, or query-string logging
- a private upstream connection to `127.0.0.1:7300`
- caller IP controls at the proxy; Coop authenticates keys, not end-user identities

## gVisor provider and future runtime classes

The integrated gVisor provider creates one reviewed `runsc` OCI workload per
job. Its capability class is `gvisor-application-kernel`; it must report
disabled networking and all limit controls, while each terminal receipt binds
the exact runtime, private-rootfs manifest, and generated OCI configuration by
SHA-256. It does not reuse the namespace backend's guest seccomp claim. Run the
verifier with `COOP_VERIFY_MINIMUM_ISOLATION=gvisor-application-kernel` when
gVisor is the required production contract. Merely placing the outer Coop
service inside an unrelated gVisor container still does not create this
per-job boundary.

The release gate uses the real OCI create/ready/execute/wait/delete lifecycle,
denies `AF_INET`, exercises timeout plus process-tree cancellation, kills the
server mid-run, reconciles stale state, switches providers, and requires zero
leaked cgroups/runtime directories. This is evidence for the exact pinned
path—not every gVisor build or host. `hardware-vm` and `confidential-vm` remain
valid future requirement classes but no built-in provider currently satisfies
them; Coop must not advertise either until a separately reviewed provider and
evidence contract exist.
