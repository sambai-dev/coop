# Standalone terminal apps

This folder already contains everything needed to use Rookhold from a terminal:

- `rookhold` starts the local service;
- `rookhold-cli` is the interactive human client; and
- `rookhold-mcp` connects Claude Code, OpenCode, Hermes, OpenClaw, and other MCP hosts.

For the smallest download, only `rookhold-cli` is needed. The same executable
starts the MCP adapter with `rookhold-cli mcp-server`; the separate
`rookhold-mcp` file remains a compatibility entry point.

Windows executables end in `.exe`. Python, Rust, `pip`, and a source checkout are
not required.

The Linux x86_64 terminal apps are musl-native, matching the service archive,
so they do not depend on a host Python installation or glibc runtime.

The current community binaries are covered by release checksums, an SBOM, and
GitHub provenance attestations, but not yet by commercial Windows or Apple
code-signing certificates. If the operating system shows an unidentified
publisher warning, verify the downloaded archive before allowing it.

## Local trusted-code demo

Start the service in one terminal. This mode is intentionally unisolated and
must stay on `127.0.0.1`.

macOS or Linux:

```bash
ROOKHOLD_SANDBOX=off ROOKHOLD_JOBS_ROOT="$PWD/.rookhold-dev/jobs" ./rookhold
```

Windows PowerShell:

```powershell
$env:ROOKHOLD_SANDBOX = "off"
$env:ROOKHOLD_JOBS_ROOT = Join-Path (Get-Location) ".rookhold-dev\jobs"
.\rookhold.exe
```

Open a second terminal and start the human client.

macOS or Linux:

```bash
ROOKHOLD_BASE_URL=http://127.0.0.1:7300 \
ROOKHOLD_API_KEY=rookhold-dev-key \
  ./rookhold-cli
```

Windows PowerShell:

```powershell
$env:ROOKHOLD_BASE_URL = "http://127.0.0.1:7300"
$env:ROOKHOLD_API_KEY = "rookhold-dev-key"
.\rookhold-cli.exe
```

Try `/mcp`, then `/run python "print(6 * 7)"`. Type `/help` for every command.

## Connect an agent CLI

Use the templates in `integrations/` and set the MCP command to the absolute
path of `rookhold-cli`, with `mcp-server` as its argument. Keep the API key in
the host environment, never in a model-visible prompt.

Adding Rookhold does not disable an agent host's built-in shell or other
execution tools. Deny those alternate routes in the host when every job must
cross Rookhold.

The local demo is not a security boundary. Before running untrusted code, read
`docs/security-boundary.md` and deploy the guarded Linux x86_64 profile on a
dedicated VM.
