# Agent integrations

Rookhold ships one universal consumer file: `rookhold-cli`. Claude Code,
OpenCode, Codex, Hermes, OpenClaw, and other MCP hosts launch that same file
with the `mcp-server` argument. The adapter does not call an LLM and does not
expose the Rookhold URL or bearer key as model-selectable arguments.

Download the single file for the current operating system from the release,
put it at a stable absolute path, and mark it executable on macOS or Linux:

```bash
chmod +x ~/.local/bin/rookhold-cli
~/.local/bin/rookhold-cli --version
```

On Windows, keep the downloaded `.exe` extension. Python, `pip`, Rust, and a
source checkout are not required. The Python wheel remains available for SDK
authors who want importable modules.

Running the file normally opens the human/operator terminal. Agent CLIs launch
`rookhold-cli mcp-server`, which exposes four tools:

| Tool | Purpose |
|---|---|
| `rookhold_run_code` | Submit one short stateless job and wait for bounded output plus its receipt |
| `rookhold_job_result` | Resume a timed-out wait or take an immediate status snapshot |
| `rookhold_job_events` | Read a bounded cursor page of persisted evidence |
| `rookhold_cancel_job` | Cancel a queued or running job |

The model never chooses the base URL, key, language allowlist, maximum code
size, maximum wait, or required isolation posture. Those are process settings:

| Variable | Default | Production guidance |
|---|---|---|
| `ROOKHOLD_BASE_URL` | `http://127.0.0.1:7300` | Use the private/TLS Rookhold endpoint |
| `ROOKHOLD_API_KEY` | none; required | Give each harness its own tenant key |
| `ROOKHOLD_MCP_MINIMUM_ISOLATION` | unset | Exact required class; use `gvisor-application-kernel` for the guarded deployment |
| `ROOKHOLD_MCP_REQUIRE_ISOLATION` | `false` | Legacy compatibility switch mapping only to `linux-shared-kernel` |
| `ROOKHOLD_MCP_ALLOWED_LANGUAGES` | `python,node,bash` | Reduce to what the agent needs |
| `ROOKHOLD_MCP_MAX_WAIT_SECONDS` | `300` | Reduce for interactive agents if desired |
| `ROOKHOLD_MCP_MAX_CODE_BYTES` | `524288` | Reduce to bound model-generated payloads further |

The adapter sends its minimum isolation requirement with every submission, so
admission and execution use the same atomic policy. It applies Rookhold's exact
class satisfaction order: gVisor and VM providers satisfy
`linux-shared-kernel`, confidential VMs satisfy `hardware-vm`, and Wasm remains
a separate branch. Terminal policy and receipt evidence must report a class
that still satisfies the configured minimum. This avoids imposing
namespace-specific seccomp/rootfs assertions on gVisor or VM providers.

## Claude Code

Claude Code supports local stdio MCP servers and environment expansion in
project-scoped `.mcp.json` files. Copy
[`claude-code/mcp.json`](claude-code/mcp.json) to `.mcp.json` at the project
root, or merge its `rookhold` entry into an existing file. Set
`ROOKHOLD_API_KEY` in the environment that launches Claude Code, plus the base
URL, minimum isolation, and language allowlist appropriate for that project.

Run `claude mcp list` and `claude mcp get rookhold`, then use `/mcp` inside
Claude Code to confirm the server and four tools. Project-scoped MCP servers
require Claude Code's trust approval before first use. The current command and
configuration behavior is documented by
[Claude Code's official MCP guide](https://code.claude.com/docs/en/mcp).

For a private user-wide registration, Claude Code also supports:

```bash
claude mcp add --transport stdio --scope user \
  --env ROOKHOLD_BASE_URL="$ROOKHOLD_BASE_URL" \
  --env ROOKHOLD_API_KEY="$ROOKHOLD_API_KEY" \
  --env ROOKHOLD_MCP_MINIMUM_ISOLATION="$ROOKHOLD_MCP_MINIMUM_ISOLATION" \
  rookhold -- rookhold-cli mcp-server
```

That command records the expanded credential in Claude Code's user
configuration; prefer `.mcp.json` environment references or your managed
configuration/secret mechanism when plaintext local config is inappropriate.

## OpenCode

OpenCode v2 runs local MCP servers over stdio from entries below
`mcp.servers`. Merge
[`opencode/opencode.snippet.json`](opencode/opencode.snippet.json) into the
project or user `opencode.json`, export the four referenced `ROOKHOLD_*`
variables, then start OpenCode normally. The server connects automatically and
its tools are grouped under the `rookhold` server name by default.

The snippet follows the current
[OpenCode v2 local-MCP schema](https://opencode.ai/v2/docs/mcp-servers/),
including its `{env:NAME}` substitution. Set `"codemode": false` on the
`rookhold` server only if you want the four tools exposed directly rather than
through OpenCode's default Code Mode.

## Hermes

1. Put the secrets and operator policy in `~/.hermes/.env`:

   ```dotenv
   ROOKHOLD_BASE_URL=https://rookhold.internal.example
   ROOKHOLD_API_KEY=replace-with-the-key-only-not-tenant-prefix
   ROOKHOLD_MCP_MINIMUM_ISOLATION=gvisor-application-kernel
   ROOKHOLD_MCP_ALLOWED_LANGUAGES=python,node
   ```

2. Merge [`hermes/config.snippet.yaml`](hermes/config.snippet.yaml) into
   `~/.hermes/config.yaml`. Replace `command: "rookhold-cli"` with the absolute
   downloaded executable path if necessary.
3. Restart Hermes and use its MCP test/reload surface. Parallel tool calls are
   supported through the adapter's bounded concurrent dispatcher; Rookhold remains
   authoritative for tenant and global admission limits.
4. Disable Hermes' local `execute_code` or terminal toolset for agents that
   must use Rookhold. Merely adding Rookhold does not stop a model from choosing an
   existing host/Docker execution tool.

## OpenClaw

1. Export `ROOKHOLD_BASE_URL`, `ROOKHOLD_API_KEY`,
   `ROOKHOLD_MCP_MINIMUM_ISOLATION=gvisor-application-kernel`, and the language allowlist into the
   Gateway service environment.
2. Merge [`openclaw/openclaw.snippet.json5`](openclaw/openclaw.snippet.json5)
   into `~/.openclaw/openclaw.json`, using the absolute `rookhold-cli` path when
   required.
3. Use OpenClaw's MCP connection test for `rookhold`, restart the Gateway, and
   inspect the effective tool policy. Sandboxed OpenClaw sessions need
   `bundle-mcp` (or the exact projected Rookhold tools) in the sandbox allowlist.
4. Deny `exec`, `process`, and provider-backed code execution for agents that
   must use Rookhold. OpenClaw's sandbox and tool policy are separate controls; an
   MCP registration alone does not replace other execution routes.

## Generic MCP host

Use this stdio definition, substituting an absolute executable path when
needed:

```json
{
  "mcpServers": {
    "rookhold": {
      "command": "rookhold-cli",
      "args": ["mcp-server"],
      "env": {
        "ROOKHOLD_BASE_URL": "https://rookhold.internal.example",
        "ROOKHOLD_API_KEY": "replace-me",
        "ROOKHOLD_MCP_MINIMUM_ISOLATION": "gvisor-application-kernel",
        "ROOKHOLD_MCP_ALLOWED_LANGUAGES": "python,node"
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

Do not configure unbounded automatic retries for `rookhold_run_code`: the recovery
table is process-local, and a restart or expiry means a lost response still
does not prove that the job was not durably submitted. Use the returned job ID
for reads and cancellation.

## Where Rookhold fits

```text
user → LLM → agent harness → rookhold-cli mcp-server → Rookhold API → policy executor
                     ↑                      │
                     └── result + job id + receipt ──┘
```

Rookhold is best for short generated snippets, tenant-facing transforms, evaluator
jobs, and any execution where an independent job record matters. A harness's
persistent Docker workspace is usually better for repository editing, package
installation, long-running services, browsers, and interactive shells. Using
both is normal: keep the persistent workspace for trusted development and
route risky/stateless execution through Rookhold.

The v0.6 adapter still accepts the former `coop_*` tool-call names and
`COOP_*` environment settings for staged upgrades, but it advertises only the
Rookhold names to new MCP sessions.
