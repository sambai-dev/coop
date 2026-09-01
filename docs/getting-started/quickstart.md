# Quickstart

Download the complete Rookhold app for your computer:

| Computer | App bundle |
|---|---|
| Windows, 64-bit | [Download for Windows](https://github.com/sambai-dev/rookhold/releases/download/v0.8.0/rookhold-x86_64-pc-windows-msvc.zip) |
| Mac with Apple silicon | [Download for Mac](https://github.com/sambai-dev/rookhold/releases/download/v0.8.0/rookhold-aarch64-apple-darwin.tar.gz) |
| Linux x86_64 | [Download for Linux](https://github.com/sambai-dev/rookhold/releases/download/v0.8.0/rookhold-x86_64-unknown-linux-musl.tar.gz) |

Extract it, then run one job. On Windows:

```powershell
.\rookhold.exe run python 'print(6 * 7)'
```

On macOS or Linux:

```bash
chmod +x rookhold rookhold-cli rookhold-mcp rookhold-verify
./rookhold run python 'print(6 * 7)'
```

The zero-configuration path starts and stops a temporary loopback service:

```text
42

status       succeeded
network      host
isolation    none
receipt      saved to .rookhold/runs/…/receipt.json

WARNING: isolation is none; this run did not contain untrusted code.
```

::: warning Trusted code only
This local mode is convenient, not contained. It has host networking and the
service account's authority. Use it only for code you trust.
:::

For untrusted code, connect the same app to a guarded Linux service and require
its observed isolation class:

```bash
export ROOKHOLD_BASE_URL=https://executor.example
export ROOKHOLD_API_KEY=replace-with-a-scoped-key
rookhold run python 'print(6 * 7)' \
  --minimum-isolation gvisor-application-kernel
```

That connected run should report `network: disabled` and
`isolation: gvisor-application-kernel`. Rookhold fails admission if the service
cannot satisfy the requested class.

Next: [install an SDK](installation.md#sdk) or
[deploy the secure Linux boundary](first-secure-deployment.md).
