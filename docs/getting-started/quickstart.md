# Quickstart

Install the command from the [v0.8.0 release](https://github.com/sambai-dev/rookhold/releases/tag/v0.8.0), put `rookhold` on `PATH`, then run:

```bash
rookhold run python 'print(6 * 7)'
```

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
