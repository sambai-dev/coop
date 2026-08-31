# Deployment

Rookhold v0.8 retains three explicit operating modes: per-job gVisor OCI on a dedicated Linux x86_64 VM (the guarded production default), the weaker Linux x86_64 namespace fallback, and an unisolated same-trust development subprocess. No mode silently satisfies a stronger requested isolation class.

## Prerequisites for isolated providers

- an x86_64 Linux host; isolated providers do not support other architectures
- Linux 5.14 or newer with a unified, writable cgroup v2 hierarchy (`cgroup.kill` and recursive `mount_setattr` are required)
- effective UID 0; the current backend does not support non-root delegation
- a dedicated VM with no unrelated workloads
- a trusted private rootfs containing the configured interpreters
- for gVisor: the exact reviewed `runsc`, `vm.max_map_count >= 4194304`, the rootfs manifest digest, and the matching `rookhold-oci-init`
- a dedicated SQLite path outside that rootfs; the database must be a regular,
  non-symlinked file with exactly one hard link
- scoped credentials (or migration-only high-entropy tenant keys), an owner-only Ed25519 signing key, and TLS/private-network ingress

Validate the exact deployment by running the hostile suite. “The process starts” and “the health check is green” are not containment tests.

## Compose on a dedicated VM

On an x86_64 Linux Docker host, the repository image constructs `/opt/rookhold/rootfs` separately from the outer container filesystem. Base images are digest-pinned and both outer runtime and job rootfs install interpreters from the same dated Debian snapshot. Compose defaults to `ROOKHOLD_SANDBOX=gvisor`, bind-mounts the reviewed runtime read-only, mounts the signing key as a file-backed secret, binds the exact manifest digest, and fails without credentials. Because bind mounts retain host ownership, the image entrypoint copies the runtime and private key into separate root-owned container-local tmpfs paths before it execs Rookhold. The strict key/runtime readers validate those staged copies. The Dockerfile refuses another production architecture. Rookhold runs as container PID 1 with host-cgroup delegation; do not add Docker's `init: true` or unrelated co-processes.

Use the guarded bootstrap. It refuses non-Linux/non-x86_64 hosts, requires an
explicit dedicated-VM acknowledgement, creates `.env` plus `.coop-runtime/`
without overwriting existing keys, downloads and verifies the pinned `runsc`,
generates an Ed25519 key plus a locally derived public-key pin, builds with
pulled bases, validates the root-owned staged key/runtime through that exact
image, computes the exact rootfs digest, establishes the gVisor map-count
floor, waits for readiness, and runs one receipt-and-attestation-checked canary
in each runtime:

```bash
ROOKHOLD_PRODUCTION_VM_ACKNOWLEDGED=true scripts/bootstrap-production.sh
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
rookhold_image=$(docker image inspect --format '{{.Id}}' "$(docker compose images -q rookhold)")
ROOKHOLD_CLIENT_KEY="$key" \
ROOKHOLD_VERIFY_CONTAINER_IMAGE="$rookhold_image" \
ROOKHOLD_VERIFY_PUBLIC_KEY_FILE="$(pwd -P)/.coop-runtime/attestation-public-key.pem" \
ROOKHOLD_VERIFY_MINIMUM_ISOLATION=gvisor-application-kernel \
  python3 scripts/verify-production.py
```

For a binary/systemd installation, set `ROOKHOLD_VERIFY_BIN` to the packaged
absolute `rookhold-verify` path instead of `ROOKHOLD_VERIFY_CONTAINER_IMAGE`. The pin
is mandatory in both modes. The verifier never obtains trust from
`/v1/attestation/public-key`.

The verifier permits plain HTTP only on loopback. Set `ROOKHOLD_VERIFY_BASE_URL` to
an HTTPS endpoint for remote ingress and `ROOKHOLD_VERIFY_LANGUAGES` to a
comma-separated subset when the deployment intentionally disables a runtime.
`ROOKHOLD_VERIFY_MINIMUM_ISOLATION` accepts the API's exact isolation-class values;
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
ROOKHOLD_CLIENT_KEY="$key" \
ROOKHOLD_VERIFY_BASE_URL=http://127.0.0.1:7300 \
ROOKHOLD_VERIFY_MINIMUM_ISOLATION=gvisor-application-kernel \
  python3 scripts/verify-python-adapter.py
```

Before admitting jobs, compare `execution.isolation_class` with the configured
minimum using the API's satisfaction order, require disabled networking and all
five limit-enforcement flags for isolated process providers, require
`storage_ready: true`, and require `scheduler.shutting_down: false`. Namespace
deployments must additionally report the private rootfs, dedicated bootstrap,
and Rookhold seccomp filter. gVisor deliberately reports `seccomp: false` for the
namespace-guest filter; its terminal policy and receipt must instead carry the
reviewed runtime, rootfs, and OCI-config SHA-256 digests. The verifier submits
the minimum atomically with every canary; confirms requested, effective,
policy, and receipt isolation records agree; then waits for each signed
attestation, downloads exact envelope/result bytes, checks every declared
digest, length, media type, receipt binding, and result field, and invokes the
packaged `rookhold-verify` against those exact bytes with the explicit pin.

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
sudo install -o root -g root -m 0755 target/release/rookhold /usr/local/bin/rookhold
sudo install -o root -g root -m 0755 \
  target/release/rookhold-sandbox-init /usr/local/bin/rookhold-sandbox-init
sudo install -o root -g root -m 0755 \
  target/release/rookhold-verify /usr/local/bin/rookhold-verify
```

```bash
sudo debootstrap \
  --variant=minbase \
  --include=python3,nodejs,bash,ca-certificates \
  bookworm /opt/rookhold/rootfs https://deb.debian.org/debian
sudo install -d -m 0755 \
  /opt/rookhold/rootfs/.pivot_old \
  /opt/rookhold/rootfs/proc \
  /opt/rookhold/rootfs/dev \
  /opt/rookhold/rootfs/work
sudo install -d -m 1777 /opt/rookhold/rootfs/tmp
sudo install -d -m 0755 /opt/rookhold/rootfs/tmp/home
sudo chown -R root:root /opt/rookhold/rootfs
sudo chmod -R go-w /opt/rookhold/rootfs
sudo install -o root -g root -m 0755 \
  target/release/rookhold-oci-init /opt/rookhold/rootfs/usr/local/bin/rookhold-oci-init
sudo python3 scripts/build-rootfs-manifest.py /opt/rookhold/rootfs
```

For reproducible deployments, replace the rolling mirror with an approved snapshot and record the package manifest and tree digest. Create empty `/.pivot_old`, `/proc`, `/dev`, `/tmp/home`, and `/work` mount points if the rootfs tool did not. Keep `/.pivot_old` empty. Do not copy the host root or mount the Rookhold data directory into the rootfs.

Set at minimum:

```bash
export ROOKHOLD_ENV=production
export ROOKHOLD_SANDBOX=gvisor
export ROOKHOLD_ROOTFS=/opt/rookhold/rootfs
export ROOKHOLD_SANDBOX_HELPER=/usr/local/bin/rookhold-sandbox-init
export ROOKHOLD_GVISOR_RUNSC=/usr/local/bin/runsc
export ROOKHOLD_GVISOR_ROOTFS_SHA256="$(sha256sum /opt/rookhold/rootfs/.coop-rootfs.manifest | awk '{print $1}')"
export ROOKHOLD_GVISOR_PLATFORM=systrap
export ROOKHOLD_ATTESTATION_MODE=sign
export ROOKHOLD_ATTESTATION_KEY_FILE=/etc/rookhold/attestation-key.pem
export ROOKHOLD_JOBS_ROOT=/var/lib/rookhold/jobs
export ROOKHOLD_DB=/var/lib/rookhold/rookhold.db
export ROOKHOLD_API_KEYS="agent-a:$(openssl rand -hex 32)"
```

Provision `/usr/local/bin/runsc` from the exact version and SHA-256 pinned in
`scripts/smoke-gvisor.sh`; do not use an unreviewed distribution package or
moving URL. Generate the signing key once with
`sudo rookhold-verify generate-key --output /etc/rookhold/attestation-key.pem`, keep it
mode `0600`, distribute the derived public key through a separate trusted
channel, and retain previous public keys across rotation. The namespace
fallback uses `ROOKHOLD_SANDBOX=ns` and the matching helper, but its advertised
minimum is only `linux-shared-kernel`.

Interpreter overrides name executable paths inside the private rootfs; an absolute override such as `/usr/bin/python3` must resolve beneath that root. Test Python, Node, and Bash canaries after every rootfs update.

Install `rookhold`, `rookhold-verify`, `rookhold-sandbox-init`, and `rookhold-oci-init` from the
same build. The namespace helper performs rootfs/PID/credential/seccomp setup;
the OCI init proves the process is inside gVisor before launching user code.
Do not substitute binaries from another release or make them writable by job
credentials.

`deploy/rookhold.service`, `deploy/rookhold.env.example`, and `deploy/Caddyfile.example` are starting templates for systemd and TLS ingress. The Caddy template caps request headers at 32 KiB, bounds header/body reads at 30 seconds, allows six minutes for the maximum five-minute `/result` wait plus transfer margin, and expires idle connections after 10 minutes. The unit rejects non-x86_64 hosts and creates `/var/lib/rookhold` with mode `0700`. Install the environment file as root with mode `0600`, fill its blank key, review paths/capabilities against your distribution, run `systemd-analyze security rookhold.service`, and execute the hostile suite before admitting traffic. `Delegate=yes` and writable cgroup v2 delegation are required.

## Local development

On macOS, Windows, non-x86_64 Linux, or x86_64 Linux without namespace prerequisites, development mode can use the subprocess backend:

```bash
ROOKHOLD_SANDBOX=off \
ROOKHOLD_JOBS_ROOT="$PWD/.rookhold-dev/jobs" \
cargo run --locked -p coop-server --bin rookhold
```

This mode is not isolated: submitted code has the service account's filesystem and network access. It enforces wall time, cancellation, and bounded output, but not requested CPU, memory, process-count, or file-size controls. At startup it canaries Python, Node.js, and Bash under the job environment; only passing runtimes appear in `/v1/capabilities`, and their exact executable paths are reused for jobs. Keep the listener on loopback and submit only code you trust. Production mode requires the conspicuous `ROOKHOLD_UNSAFE_ALLOW_NAIVE=true` acknowledgement for an explicit `off` setting; that acknowledgement does not make the mode safer.

On PowerShell, set `$env:ROOKHOLD_SANDBOX = "off"` and `$env:ROOKHOLD_JOBS_ROOT = Join-Path (Get-Location) ".rookhold-dev\jobs"` before `cargo run --locked -p coop-server --bin rookhold`.

## Prebuilt archives

Before using a moving `releases/latest` URL, verify that it resolves to v0.8.0 or newer. Older release lines are unsupported for new deployments. Release archives are named by Rust target and include documentation, deploy templates, integration templates, SDK source, and `rookhold-verify`. The Linux archive also includes both execution init helpers and the legacy `coop*` executable aliases.

The following commands require a current [GitHub CLI](https://cli.github.com/) with artifact-attestation support; authenticate it according to your organization's policy before downloading. For Linux x86_64:

```bash
set -euo pipefail
version=0.8.0
asset=rookhold-x86_64-unknown-linux-musl.tar.gz
gh release download "v${version}" --repo sambai-dev/rookhold \
  --pattern "$asset" --pattern SHA256SUMS
verify_github_asset() {
  gh release verify-asset "v${version}" "$1" --repo sambai-dev/rookhold
  gh attestation verify "$1" \
    --repo sambai-dev/rookhold \
    --signer-workflow sambai-dev/rookhold/.github/workflows/release.yml \
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
  rookhold-x86_64-unknown-linux-musl/rookhold /usr/local/bin/rookhold
sudo install -o root -g root -m 0755 \
  rookhold-x86_64-unknown-linux-musl/rookhold-sandbox-init \
  /usr/local/bin/rookhold-sandbox-init
sudo install -o root -g root -m 0755 \
  rookhold-x86_64-unknown-linux-musl/rookhold-oci-init \
  /usr/local/bin/rookhold-oci-init
sudo install -o root -g root -m 0755 \
  rookhold-x86_64-unknown-linux-musl/rookhold-verify \
  /usr/local/bin/rookhold-verify
```

This installs binaries only. Build the private rootfs, configuration, service, and TLS ingress described above before starting production.

For an Apple-silicon macOS development installation:

```bash
set -euo pipefail
version=0.8.0
asset=rookhold-aarch64-apple-darwin.tar.gz
gh release download "v${version}" --repo sambai-dev/rookhold \
  --pattern "$asset" --pattern SHA256SUMS
verify_github_asset() {
  gh release verify-asset "v${version}" "$1" --repo sambai-dev/rookhold
  gh attestation verify "$1" \
    --repo sambai-dev/rookhold \
    --signer-workflow sambai-dev/rookhold/.github/workflows/release.yml \
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
install -m 0755 rookhold-aarch64-apple-darwin/rookhold "$HOME/.local/bin/rookhold"
install -m 0755 rookhold-aarch64-apple-darwin/rookhold-verify "$HOME/.local/bin/rookhold-verify"
```

For an x86_64 Windows development installation in PowerShell:

```powershell
$ErrorActionPreference = "Stop"
$version = "0.8.0"
$asset = "rookhold-x86_64-pc-windows-msvc.zip"
gh release download "v$version" --repo sambai-dev/rookhold `
    --pattern $asset --pattern SHA256SUMS
if ($LASTEXITCODE -ne 0) { throw "release download failed" }
gh release verify-asset "v$version" SHA256SUMS --repo sambai-dev/rookhold
if ($LASTEXITCODE -ne 0) { throw "release verification failed for SHA256SUMS" }
gh attestation verify SHA256SUMS `
    --repo sambai-dev/rookhold `
    --signer-workflow sambai-dev/rookhold/.github/workflows/release.yml `
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
gh release verify-asset "v$version" $asset --repo sambai-dev/rookhold
if ($LASTEXITCODE -ne 0) { throw "release verification failed for $asset" }
gh attestation verify $asset `
    --repo sambai-dev/rookhold `
    --signer-workflow sambai-dev/rookhold/.github/workflows/release.yml `
    --source-ref "refs/tags/v$version" `
    --predicate-type https://slsa.dev/provenance/v1 `
    --deny-self-hosted-runners
if ($LASTEXITCODE -ne 0) { throw "workflow provenance failed for $asset" }
Expand-Archive -Path $asset -DestinationPath . -Force
New-Item -ItemType Directory -Force "$HOME\bin" | Out-Null
Copy-Item "rookhold-x86_64-pc-windows-msvc\rookhold.exe" "$HOME\bin\rookhold.exe"
Copy-Item "rookhold-x86_64-pc-windows-msvc\rookhold-verify.exe" "$HOME\bin\rookhold-verify.exe"
```

Add the chosen user-local binary directory to `PATH`. The checksum file and combined SPDX JSON SBOM are release assets. `SHA256SUMS` names and hashes the other ten assets; its own release and workflow attestations authenticate the downloaded manifest. The SPDX inventories the three direct CLI files plus freshly extracted content from the six built archives/packages, and its separate SBOM attestation binds that document to those nine payload digests. The constrained provenance check authenticates the expected release workflow and tag for the exact bytes; it does not turn a development subprocess into isolation or gVisor into trusted hardware.

The Apple-silicon macOS and x86_64 Windows archives run the local-development subprocess backend only. Non-x86_64 Linux source builds have the same limitation. They are useful for trusted-code integration work, not production containment. A production x86_64 Linux binary installation still needs the private rootfs, cgroup/systemd setup, keys, TLS ingress, and hostile-suite validation described above.

## TLS proxy requirements

- TLS 1.2 or newer, with certificate validation by clients
- a request-header deadline no longer than 30 seconds, a bounded header size (the Caddy template uses 32 KiB), and body/connection limits compatible with Rookhold's API limits
- WebSocket upgrade support and long enough idle timeout for the maximum job wall time
- a response-write budget longer than the maximum 300-second `/result` wait (the Caddy template uses six minutes)
- no bearer-token, body, or query-string logging
- a private upstream connection to `127.0.0.1:7300`
- caller IP controls at the proxy; Rookhold authenticates keys, not end-user identities

## gVisor provider and future runtime classes

The integrated gVisor provider creates one reviewed `runsc` OCI workload per
job. Its capability class is `gvisor-application-kernel`; it must report
disabled networking and all limit controls, while each terminal receipt binds
the exact runtime, private-rootfs manifest, and generated OCI configuration by
SHA-256. It does not reuse the namespace backend's guest seccomp claim. Run the
verifier with `ROOKHOLD_VERIFY_MINIMUM_ISOLATION=gvisor-application-kernel` when
gVisor is the required production contract. Merely placing the outer Rookhold
service inside an unrelated gVisor container still does not create this
per-job boundary.

The release gate uses the real OCI create/ready/execute/wait/delete lifecycle,
denies `AF_INET`, exercises timeout plus process-tree cancellation, kills the
server mid-run, reconciles stale state, switches providers, and requires zero
leaked cgroups/runtime directories. This is evidence for the exact pinned
path—not every gVisor build or host. `hardware-vm` and `confidential-vm` remain
valid future requirement classes but no built-in provider currently satisfies
them; Rookhold must not advertise either until a separately reviewed provider and
evidence contract exist.
