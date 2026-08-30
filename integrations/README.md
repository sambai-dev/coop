# Agent integrations

Coop ships one universal integration surface: the `coop-mcp` stdio server in
the Python SDK wheel. Hermes, OpenClaw, Codex, Claude Code, and other MCP hosts
can all launch the same executable. The adapter does not call an LLM and does
not expose the Coop URL or bearer key as model-selectable arguments.

Install it into an operator-owned virtual environment:

```bash
python -m venv ~/.local/share/coop-mcp
~/.local/share/coop-mcp/bin/python -m pip install --no-deps ./sdks/python
```

On Windows, the executable is
`%USERPROFILE%\.local\share\coop-mcp\Scripts\coop-mcp.exe`. Use that absolute
path as the MCP `command` when the harness does not inherit the virtual
environment's `PATH`.

The server exposes four tools:

| Tool | Purpose |
|---|---|
| `coop_run_code` | Submit one short stateless job and wait for bounded output plus its receipt |
| `coop_job_result` | Resume a timed-out wait or take an immediate status snapshot |
| `coop_job_events` | Read a bounded cursor page of persisted evidence |
| `coop_cancel_job` | Cancel a queued or running job |

The model never chooses the base URL, key, language allowlist, maximum code
size, maximum wait, or required isolation posture. Those are process settings:

| Variable | Default | Production guidance |
|---|---|---|
| `COOP_BASE_URL` | `http://127.0.0.1:7300` | Use the private/TLS Coop endpoint |
| `COOP_API_KEY` | none; required | Give each harness its own tenant key |
| `COOP_MCP_MINIMUM_ISOLATION` | unset | Exact required class; use `gvisor-application-kernel` for the guarded deployment |
| `COOP_MCP_REQUIRE_ISOLATION` | `false` | Legacy compatibility switch mapping only to `linux-shared-kernel` |
| `COOP_MCP_ALLOWED_LANGUAGES` | `python,node,bash` | Reduce to what the agent needs |
| `COOP_MCP_MAX_WAIT_SECONDS` | `300` | Reduce for interactive agents if desired |
| `COOP_MCP_MAX_CODE_BYTES` | `524288` | Reduce to bound model-generated payloads further |

The adapter sends its minimum isolation requirement with every submission, so
admission and execution use the same atomic policy. It applies Coop's exact
class satisfaction order: gVisor and VM providers satisfy
`linux-shared-kernel`, confidential VMs satisfy `hardware-vm`, and Wasm remains
a separate branch. Terminal policy and receipt evidence must report a class
that still satisfies the configured minimum. This avoids imposing
namespace-specific seccomp/rootfs assertions on gVisor or VM providers.

## Hermes

1. Put the secrets and operator policy in `~/.hermes/.env`:

   ```dotenv
   COOP_BASE_URL=https://coop.internal.example
   COOP_API_KEY=replace-with-the-key-only-not-tenant-prefix
   COOP_MCP_MINIMUM_ISOLATION=gvisor-application-kernel
   COOP_MCP_ALLOWED_LANGUAGES=python,node
   ```

2. Merge [`hermes/config.snippet.yaml`](hermes/config.snippet.yaml) into
   `~/.hermes/config.yaml`. Replace `command: "coop-mcp"` with the absolute
   virtual-environment executable if necessary.
3. Restart Hermes and use its MCP test/reload surface. Parallel tool calls are
   supported through the adapter's bounded concurrent dispatcher; Coop remains
   authoritative for tenant and global admission limits.
4. Disable Hermes' local `execute_code` or terminal toolset for agents that
   must use Coop. Merely adding Coop does not stop a model from choosing an
   existing host/Docker execution tool.

## OpenClaw

1. Export `COOP_BASE_URL`, `COOP_API_KEY`,
   `COOP_MCP_MINIMUM_ISOLATION=gvisor-application-kernel`, and the language allowlist into the
   Gateway service environment.
2. Merge [`openclaw/openclaw.snippet.json5`](openclaw/openclaw.snippet.json5)
   into `~/.openclaw/openclaw.json`, using the absolute `coop-mcp` path when
   required.
3. Run `openclaw mcp doctor coop --probe`, restart the Gateway, and inspect the
   effective tool policy. Sandboxed OpenClaw sessions need `bundle-mcp` (or the
   exact projected Coop tools) in the sandbox allowlist.
4. Deny `exec`, `process`, and provider-backed code execution for agents that
   must use Coop. OpenClaw's sandbox and tool policy are separate controls; an
   MCP registration alone does not replace other execution routes.

## Generic MCP host

Use this stdio definition, substituting an absolute executable path when
needed:

```json
{
  "mcpServers": {
    "coop": {
      "command": "coop-mcp",
      "args": [],
      "env": {
        "COOP_BASE_URL": "https://coop.internal.example",
        "COOP_API_KEY": "replace-me",
        "COOP_MCP_MINIMUM_ISOLATION": "gvisor-application-kernel",
        "COOP_MCP_ALLOWED_LANGUAGES": "python,node"
      }
    }
  }
}
```

Use the host's secret-reference mechanism instead of literal credentials when
it has one. Set its per-call timeout above the adapter wait budget (330 seconds
for the default maximum). The adapter reuses one UUID for its initial submit
and one ambiguous HTTP retry. If both acknowledgements are lost, an identical
call to the same running adapter can reconcile that UUID for ten minutes; the
bounded 1,024-entry table is tenant-, policy-, and job-spec-scoped and fails
closed at capacity. A valid acknowledged job ID clears the exact entry so a
later intentional identical call remains a new job.

Do not configure unbounded automatic retries for `coop_run_code`: the recovery
table is process-local, and a restart or expiry means a lost response still
does not prove that the job was not durably submitted. Use the returned job ID
for reads and cancellation.

## Where Coop fits

```text
user → LLM → agent harness → coop-mcp → Coop API → policy executor
                     ↑                      │
                     └── result + job id + receipt ──┘
```

Coop is best for short generated snippets, tenant-facing transforms, evaluator
jobs, and any execution where an independent job record matters. A harness's
persistent Docker workspace is usually better for repository editing, package
installation, long-running services, browsers, and interactive shells. Using
both is normal: keep the persistent workspace for trusted development and
route risky/stateless execution through Coop.
