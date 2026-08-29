# Deployment

Coop v0.3 has two supported operating postures: an explicitly unisolated local-development process and the Linux x86_64 namespace backend on a dedicated x86_64 VM. A hardened external runtime is the intended production-grade evolution, but gVisor/OCI is not bundled in this release.

## Prerequisites for the namespace backend

- an x86_64 Linux host; the namespace bootstrap and seccomp policy do not support other architectures
- Linux 5.14 or newer with a unified, writable cgroup v2 hierarchy (`cgroup.kill` and recursive `mount_setattr` are required)
- effective UID 0; the current backend does not support non-root delegation
- a dedicated VM with no unrelated workloads
- a trusted private rootfs containing the configured interpreters
- a dedicated SQLite path outside that rootfs; the database must be a regular,
  non-symlinked file with exactly one hard link
- high-entropy tenant keys and a TLS/private-network ingress

Validate the exact deployment by running the hostile suite. “The process starts” and “the health check is green” are not containment tests.

## Compose on a dedicated VM

On an x86_64 Linux Docker host, the repository image constructs `/opt/coop/rootfs` separately from the outer container filesystem. Its base images are digest-pinned and both the outer runtime and job rootfs install interpreters from the same dated Debian snapshot. Compose sets `COOP_ROOTFS` and fails if no API key is provided. The Dockerfile refuses to produce a production image on another architecture. The supported container contract runs Coop directly as container PID 1 and grants the container's cgroup delegation to that single service; do not add Docker's `init: true` or unrelated co-processes. This is a deployment contract, not a claim that the runtime can reliably identify and reject every extra process already placed in the delegated cgroup. Sandbox workloads still use Coop's dedicated PID-namespace init/reaper.

Use the guarded bootstrap. It refuses non-Linux/non-x86_64 hosts, requires an
explicit dedicated-VM acknowledgement, creates `.env` with mode `0600` without
overwriting it, builds with pulled bases, waits for readiness, verifies
authenticated posture, and runs one receipt-checked canary in each runtime:

```bash
COOP_PRODUCTION_VM_ACKNOWLEDGED=true scripts/bootstrap-production.sh
```

The guard is deliberate: `.env` contains bearer credentials. If it already
exists, the script requires a non-empty `agent-a:key` entry and leaves the file
unchanged. Review the script and Compose file before running them on your VM.

Confirm posture with an authenticated request:

```bash
curl --fail-with-body \
  -H "Authorization: Bearer $key" \
  http://127.0.0.1:7300/v1/status
```

To repeat the full posture and canary gate without rebuilding, extract the key
into `COOP_CLIENT_KEY` through your secret manager and run:

```bash
COOP_CLIENT_KEY="$key" \
COOP_VERIFY_MINIMUM_ISOLATION=linux-shared-kernel \
  python3 scripts/verify-production.py
```

The verifier permits plain HTTP only on loopback. Set `COOP_VERIFY_BASE_URL` to
an HTTPS endpoint for remote ingress and `COOP_VERIFY_LANGUAGES` to a
comma-separated subset when the deployment intentionally disables a runtime.
`COOP_VERIFY_MINIMUM_ISOLATION` accepts the API's exact isolation-class values;
it defaults to `linux-shared-kernel`. That minimum also accepts gVisor,
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
COOP_VERIFY_MINIMUM_ISOLATION=linux-shared-kernel \
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
the minimum atomically with every canary and confirms the requested, effective,
policy, and receipt isolation records all agree.

The Compose service is privileged because the in-tree backend creates namespaces, mounts, and cgroups. On Linux, a privileged-container compromise is effectively a host compromise. Run this only inside the dedicated VM; never add the Docker socket, cloud credentials, home directories, or unrelated volumes.

The service publishes only `127.0.0.1:7300`. To serve remote clients, place a TLS proxy on the VM and limit its client network. Preserve `Authorization` and WebSocket upgrade headers, disable request-body logging, and avoid logging query strings.

## Bare-metal private rootfs

Source deployments must build and patch their own rootfs. On Debian-family hosts, `debootstrap` provides a functional starting point; use an approved snapshot mirror rather than the rolling mirror below when reproducibility is required:

Build and install both release binaries from one checkout:

```bash
cargo build --locked --release -p coop-server -p coop-exec --bins
sudo install -o root -g root -m 0755 target/release/coop /usr/local/bin/coop
sudo install -o root -g root -m 0755 \
  target/release/coop-sandbox-init /usr/local/bin/coop-sandbox-init
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
```

For reproducible deployments, replace the rolling mirror with an approved snapshot and record the package manifest and tree digest. Create empty `/.pivot_old`, `/proc`, `/dev`, `/tmp/home`, and `/work` mount points if the rootfs tool did not. Keep `/.pivot_old` empty. Do not copy the host root or mount the Coop data directory into the rootfs.

Set at minimum:

```bash
export COOP_ENV=production
export COOP_SANDBOX=ns
export COOP_ROOTFS=/opt/coop/rootfs
export COOP_SANDBOX_HELPER=/usr/local/bin/coop-sandbox-init
export COOP_JOBS_ROOT=/var/lib/coop/jobs
export COOP_DB=/var/lib/coop/coop.db
export COOP_API_KEYS="agent-a:$(openssl rand -hex 32)"
```

Interpreter overrides name executable paths inside the private rootfs; an absolute override such as `/usr/bin/python3` must resolve beneath that root. Test Python, Node, and Bash canaries after every rootfs update.

Install `coop` and the matching `coop-sandbox-init` from the same build. The helper is a fresh single-threaded bootstrap process used to perform namespace, rootfs, PID 1, credential, and seccomp setup without forking an allocation-heavy multithreaded server runtime. Do not substitute a helper from another release or make it writable by the service's job credentials.

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

Before using a moving `releases/latest` URL, verify that it resolves to v0.3.0 or newer. Older release lines are unsupported for new deployments. Release archives are named by Rust target and include documentation, deploy templates, integration templates, and SDK source. The Linux archive also includes the matching helper.

The following commands require a current [GitHub CLI](https://cli.github.com/) with artifact-attestation support; authenticate it according to your organization's policy before downloading. For Linux x86_64:

```bash
version=0.3.0
asset=coop-x86_64-unknown-linux-musl.tar.gz
gh release download "v${version}" --repo sambai-dev/coop \
  --pattern "$asset" --pattern SHA256SUMS
sha256sum --check --ignore-missing SHA256SUMS
gh attestation verify "$asset" --repo sambai-dev/coop
tar -xzf "$asset"
sudo install -o root -g root -m 0755 \
  coop-x86_64-unknown-linux-musl/coop /usr/local/bin/coop
sudo install -o root -g root -m 0755 \
  coop-x86_64-unknown-linux-musl/coop-sandbox-init \
  /usr/local/bin/coop-sandbox-init
```

This installs binaries only. Build the private rootfs, configuration, service, and TLS ingress described above before starting production.

For an Apple-silicon macOS development installation:

```bash
version=0.3.0
asset=coop-aarch64-apple-darwin.tar.gz
gh release download "v${version}" --repo sambai-dev/coop \
  --pattern "$asset" --pattern SHA256SUMS
expected=$(awk -v file="$asset" '$2 == file { print $1 }' SHA256SUMS)
actual=$(shasum -a 256 "$asset" | awk '{ print $1 }')
test -n "$expected" && test "$actual" = "$expected"
gh attestation verify "$asset" --repo sambai-dev/coop
tar -xzf "$asset"
install -d "$HOME/.local/bin"
install -m 0755 coop-aarch64-apple-darwin/coop "$HOME/.local/bin/coop"
```

For an x86_64 Windows development installation in PowerShell:

```powershell
$version = "0.3.0"
$asset = "coop-x86_64-pc-windows-msvc.zip"
gh release download "v$version" --repo sambai-dev/coop `
    --pattern $asset --pattern SHA256SUMS
$line = (Select-String -Path SHA256SUMS -Pattern ([regex]::Escape($asset) + '$')).Line
if (-not $line) { throw "checksum is missing for $asset" }
$expected = ($line -split '\s+')[0].ToLowerInvariant()
$actual = (Get-FileHash -Algorithm SHA256 $asset).Hash.ToLowerInvariant()
if (-not $expected -or $actual -ne $expected) { throw "checksum mismatch: $asset" }
gh attestation verify $asset --repo sambai-dev/coop
Expand-Archive -Path $asset -DestinationPath . -Force
New-Item -ItemType Directory -Force "$HOME\bin" | Out-Null
Copy-Item "coop-x86_64-pc-windows-msvc\coop.exe" "$HOME\bin\coop.exe"
```

Add the chosen user-local binary directory to `PATH`. The checksum file and SPDX JSON SBOM are release assets. Verification proves which GitHub workflow produced the artifact; it does not turn the shared-kernel execution backend into a stronger boundary.

The Apple-silicon macOS and x86_64 Windows archives run the local-development subprocess backend only. Non-x86_64 Linux source builds have the same limitation. They are useful for trusted-code integration work, not production containment. A production x86_64 Linux binary installation still needs the private rootfs, cgroup/systemd setup, keys, TLS ingress, and hostile-suite validation described above.

## TLS proxy requirements

- TLS 1.2 or newer, with certificate validation by clients
- a request-header deadline no longer than 30 seconds, a bounded header size (the Caddy template uses 32 KiB), and body/connection limits compatible with Coop's API limits
- WebSocket upgrade support and long enough idle timeout for the maximum job wall time
- a response-write budget longer than the maximum 300-second `/result` wait (the Caddy template uses six minutes)
- no bearer-token, body, or query-string logging
- a private upstream connection to `127.0.0.1:7300`
- caller IP controls at the proxy; Coop authenticates keys, not end-user identities

## gVisor and other external runtimes

The integrated gVisor provider creates one reviewed `runsc` OCI workload per
job. Its capability class is `gvisor-application-kernel`; it must report
disabled networking and all limit controls, while each terminal receipt binds
the exact runtime, private-rootfs manifest, and generated OCI configuration by
SHA-256. It does not reuse the namespace backend's guest seccomp claim. Run the
verifier with `COOP_VERIFY_MINIMUM_ISOLATION=gvisor-application-kernel` when
gVisor is the required production contract. Merely placing the outer Coop
service inside an unrelated gVisor container still does not create this
per-job boundary.
