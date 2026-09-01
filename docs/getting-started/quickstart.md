# Quickstart

The one-command flow is available on `main` and will ship in v0.8.0. Build the
current checkout, then run:

```bash
cargo build --locked -p coop-server --bin rookhold
cp target/debug/rookhold ./rookhold
rookhold run python 'print(6 * 7)'
```

The current published release is
[v0.7.1](https://github.com/sambai-dev/rookhold/releases/tag/v0.7.1) and still
uses the explicit service-plus-client flow.

Without configured service variables, the command manages a temporary
loopback-only development service and prints an unavoidable warning:

```text
42

status       succeeded
network      host
isolation    none
receipt      saved to .rookhold/runs/…/receipt.json
```

That path is for trusted code only. For an existing service:

```bash
export ROOKHOLD_BASE_URL=https://rookhold.example.internal
export ROOKHOLD_API_KEY=replace-with-a-scoped-key
rookhold check
rookhold run python 'print(6 * 7)'
```

Next: [install an SDK](installation.md) or [deploy the secure Linux boundary](first-secure-deployment.md).
