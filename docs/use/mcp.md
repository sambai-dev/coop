# MCP

The Rookhold app can configure maintained hosts safely:

```bash
rookhold setup claude-code
rookhold setup hermes
rookhold setup opencode
```

The command finds the normal configuration file, shows a diff, creates a
timestamped backup, writes only after confirmation, keeps the API key in the
environment, and runs `rookhold check` afterward.

The standalone existing-server client can serve MCP over stdio. Your host
launches it with one argument:

```text
rookhold-cli mcp-server
```

The adapter exposes four tools:

| Tool | What it does |
|---|---|
| `rookhold_run_code` | Submit one short job and wait for bounded output plus its receipt |
| `rookhold_job_result` | Resume a wait or take an immediate job snapshot |
| `rookhold_job_events` | Read a bounded cursor page of persisted evidence |
| `rookhold_cancel_job` | Cancel a queued or running job |

The model cannot choose the Rookhold URL, bearer key, language allowlist,
maximum code size, maximum wait, or minimum isolation. Configure those as
process environment variables:

```bash
export ROOKHOLD_BASE_URL=https://rookhold.internal.example
export ROOKHOLD_API_KEY=replace-with-a-scoped-key
export ROOKHOLD_MCP_MINIMUM_ISOLATION=gvisor-application-kernel
export ROOKHOLD_MCP_ALLOWED_LANGUAGES=python,node
```

To configure manually, merge one of the maintained host templates and point
its command at the included or separately downloaded `rookhold-cli` file:

- [Claude Code template](https://github.com/sambai-dev/rookhold/blob/main/integrations/claude-code/mcp.json)
- [OpenCode template](https://github.com/sambai-dev/rookhold/blob/main/integrations/opencode/opencode.snippet.json)
- [Hermes template](https://github.com/sambai-dev/rookhold/blob/main/integrations/hermes/config.snippet.yaml)
- [OpenClaw template](https://github.com/sambai-dev/rookhold/blob/main/integrations/openclaw/openclaw.snippet.json5)
- [Generic MCP configuration](https://github.com/sambai-dev/rookhold/blob/main/integrations/README.md#generic-mcp-host)

Adding Rookhold does not disable a host's built-in shell. Remove or deny other
execution tools when a model must cross only the Rookhold boundary.

## Verify the connection

1. Start the host with the four `ROOKHOLD_*` values in its environment.
2. Open its MCP status surface and confirm the `rookhold` server is connected.
3. Confirm exactly four Rookhold tools are advertised.
4. Submit a trusted canary and inspect its result, isolation, and receipt.

Claude Code users can run `claude mcp list`, `claude mcp get rookhold`, and
then `/mcp`. OpenCode, Hermes, and other hosts expose an equivalent MCP status
or reload surface.

::: warning Rookhold is not automatically mandatory
Adding the MCP server does not disable a host's built-in shell, terminal, or
other execution routes. Remove or deny those tools when a model must cross
only the Rookhold boundary.
:::

See the [complete integration contract](https://github.com/sambai-dev/rookhold/tree/main/integrations)
for timeout, retry, Tasks, and host-specific policy details.
