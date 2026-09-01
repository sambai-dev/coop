# Installation

Choose the Rookhold surface that matches what you are doing.

## Rookhold app

Use the complete app when you want `rookhold run`, a local trusted-code service,
the persistent server, setup commands, the remote client, MCP, and the verifier.

| Computer | App bundle |
|---|---|
| Windows, 64-bit | [Download for Windows](https://github.com/sambai-dev/rookhold/releases/download/v0.8.0/rookhold-x86_64-pc-windows-msvc.zip) |
| Mac with Apple silicon | [Download for Mac](https://github.com/sambai-dev/rookhold/releases/download/v0.8.0/rookhold-aarch64-apple-darwin.tar.gz) |
| Linux x86_64 | [Download for Linux](https://github.com/sambai-dev/rookhold/releases/download/v0.8.0/rookhold-x86_64-unknown-linux-musl.tar.gz) |

The exact release, checksums, SBOM, provenance, SDK artifacts, and standalone
clients are on the [v0.8.0 release](https://github.com/sambai-dev/rookhold/releases/tag/v0.8.0).

## SDK

Use an SDK when an application submits jobs to a Rookhold endpoint.

```bash
pip install rookhold
npm install rookhold
```

These packages are clients. Installing them does not create the guarded Linux
execution boundary.

## Standalone client

Use `rookhold-cli` when a person or MCP host already has a Rookhold endpoint and
does not need the service or local-run workflow.

| Computer | Client |
|---|---|
| Windows, 64-bit | [Download the Windows client](https://github.com/sambai-dev/rookhold/releases/download/v0.8.0/rookhold-cli-x86_64-pc-windows-msvc.exe) |
| Mac with Apple silicon | [Download the Mac client](https://github.com/sambai-dev/rookhold/releases/download/v0.8.0/rookhold-cli-aarch64-apple-darwin) |
| Linux x86_64 | [Download the Linux client](https://github.com/sambai-dev/rookhold/releases/download/v0.8.0/rookhold-cli-x86_64-unknown-linux-gnu) |

Mac and Linux users must mark direct executable downloads with `chmod +x`.
Keep credentials in the environment or a managed secret mechanism.

## Verify a release

Before running an unsigned community binary, verify its `SHA256SUMS` entry and
GitHub provenance. The [deployment guide](../deployment.md) contains exact
Linux, macOS, and Windows verification commands.
