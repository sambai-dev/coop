# Agent and harness integration

Coop is the execution layer, not the model or agent loop. An LLM is optional
for Coop itself: applications can call the HTTP API or SDKs directly. When an
agent is involved, the harness decides when to call a tool and Coop owns the
job's execution policy and evidence.

```text
LLM ⇄ Hermes / OpenClaw / another MCP host ⇄ coop-mcp ⇄ Coop
```

The Python SDK package installs a dependency-free `coop-mcp` stdio server.
Follow the runnable templates and operator-policy guidance in
[`integrations/`](../integrations/README.md).

## Choosing Coop or a harness sandbox

Use Coop when the unit of work is a short, stateless Python, Node.js, or Bash
job and you need an independent tenant boundary, server-clamped limits,
bounded output, cancellation, and a retained receipt. Use the harness's native
sandbox when the task needs a persistent filesystem, package installation,
interactive commands, a browser, inbound ports, or a long-running service.

They are complementary. For example, an agent can edit trusted project files
inside its persistent workspace, then send a generated migration, evaluator,
or user-supplied transform to Coop. The parent trace should retain the Coop
job ID.

## Trusted adapter boundary

The adapter configuration is operator-owned. Never let the model provide:

- the Coop base URL or bearer key
- a different tenant identity
- the isolation requirement
- the allowed-language set or adapter ceilings

Give every harness or agent identity a separate Coop tenant key. This makes
rate/concurrency policy and investigation attributable, and lets operators
revoke one integration without rotating every client.

If a harness still exposes local shell, terminal, file-execution, or another
code tool, the model can bypass Coop. Remove or deny those tools for workflows
where Coop is mandatory. Tool visibility and execution placement are harness
policy; Coop can only govern jobs that reach its API.

## Failure semantics

`coop_run_code` submits once and never transparently retries submission. If its
wait budget expires, it returns `complete: false` with the durable `job_id`.
Call `coop_job_result` or `coop_cancel_job` with that ID. Direct SDK callers can
use an `Idempotency-Key` (maximum 128 visible ASCII bytes) and explicitly opt
into one ambiguous transport retry; the same key and canonical job spec then
resolve to the original job.

Tool results include both MCP structured content and the same JSON serialized
as text for older hosts. Execution failures such as a nonzero exit, timeout,
OOM kill, or policy violation are successful tool transport results with a
terminal Coop status; connection, authentication, validation, and adapter
policy failures use MCP `isError: true`.

Terminal MCP results also carry `attestation` status metadata from the job
detail: availability, key ID, content digests and sizes, media type, and the two
tenant-scoped download paths. The adapter deliberately does not embed the DSSE
envelope or result artifact in MCP output, and it does not label signatures as
verified. A trusted host can download the exact files through an SDK, retain
them with the parent trace, and invoke `coop-verify verify` offline using an
independently pinned operator public key. Treat the public-key API as discovery,
not trust bootstrap; a successful verification still requires the host to
evaluate the authenticated `outcome` and `event_chain_complete` fields.

## Production checklist

- run a reviewed provider on the dedicated Linux x86_64 VM described in
  [deployment](deployment.md)
- use a private or TLS Coop endpoint and an integration-specific tenant key
- set `COOP_MCP_REQUIRE_ISOLATION=true` for a minimum
  `linux-shared-kernel` boundary, or set `COOP_MCP_MINIMUM_ISOLATION` to the
  exact stronger class the workflow requires
- restrict `COOP_MCP_ALLOWED_LANGUAGES` and adapter ceilings
- set the MCP host timeout above the intended Coop wait budget
- disable alternate execution tools when Coop is mandatory
- probe the MCP server, then run a canary through the actual harness
- retain the job ID in the parent agent trace and inspect the terminal
  `isolation_class` and receipt evidence
