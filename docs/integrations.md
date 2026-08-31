# Agent and harness integration

Rookhold is the execution layer, not the model or agent loop. An LLM is optional
for Rookhold itself: applications can call the HTTP API or SDKs directly. When an
agent is involved, the harness decides when to call a tool and Rookhold owns the
job's execution policy and evidence.

```text
LLM ⇄ Claude Code / OpenCode / another MCP host ⇄ rookhold-cli mcp-server ⇄ Rookhold
```

The single-file `rookhold-cli` download starts its dependency-free stdio server
with the `mcp-server` argument. It serves stateless MCP 2026 discovery and
opt-in Tasks alongside the legacy initialize flow, with bounded concurrent
requests and cancellation. The Python package remains an optional SDK path.

Rookhold release-checks Claude Code, OpenCode, and Hermes. Configure one with a
preview, confirmation, timestamped backup, and connection check:

```bash
rookhold setup claude-code
rookhold setup opencode
rookhold setup hermes
```

The runnable templates and operator-policy guidance live in
[`integrations/`](https://github.com/sambai-dev/rookhold/tree/main/integrations).
The host inherits `ROOKHOLD_BASE_URL` and `ROOKHOLD_API_KEY`; setup never copies
the secret value into the host file.

## Choosing Rookhold or a harness sandbox

Use Rookhold when the unit of work is a short, stateless Python, Node.js, or Bash
job and you need an independent tenant boundary, server-clamped limits,
bounded output, cancellation, and a retained receipt. Use the harness's native
sandbox when the task needs a persistent filesystem, package installation,
interactive commands, a browser, inbound ports, or a long-running service.

They are complementary. For example, an agent can edit trusted project files
inside its persistent workspace, then send a generated migration, evaluator,
or user-supplied transform to Rookhold. The parent trace should retain the Rookhold
job ID.

## Trusted adapter boundary

The adapter configuration is operator-owned. Never let the model provide:

- the Rookhold base URL or bearer key
- a different tenant identity
- the isolation requirement
- the allowed-language set or adapter ceilings

Give every harness or agent identity a separate Rookhold tenant key. This makes
rate/concurrency policy and investigation attributable, and lets operators
revoke one integration without rotating every client.

If a harness still exposes local shell, terminal, file-execution, or another
code tool, the model can bypass Rookhold. Remove or deny those tools for workflows
where Rookhold is mandatory. Tool visibility and execution placement are harness
policy; Rookhold can only govern jobs that reach its API.

## Failure semantics

`rookhold_run_code` gives every submission a UUID idempotency key and permits one
ambiguous HTTP retry with that key. If both submission acknowledgements are
lost, the running adapter retains the key for ten minutes under an opaque HMAC
fingerprint of the target tenant, submission policy, and normalized job spec.
The next matching call reuses the unresolved key; a different tenant, policy,
code, stdin, language, or limit gets a different key. A valid acknowledged
`job_id` resolves and removes only that exact entry, so a later intentional
matching call creates a new job.

The process-local reconciliation table reserves space before submitting, holds
at most 1,024 active or unresolved operations, and fails a new distinct call
closed rather than evicting an unexpired ambiguity. A restart or ten-minute
expiry ends this recovery window, so hosts must not layer unbounded automatic
retries over `rookhold_run_code`. Concurrent matching calls that started before an
ambiguity retain distinct keys; while one unresolved key is actively being
reconciled, another indistinguishable call fails closed. Wait duration and MCP
Task response mode do not change the submitted job fingerprint.

If the post-submit wait budget expires, the adapter returns `complete: false`
with the acknowledged durable `job_id`. Call `rookhold_job_result` or
`rookhold_cancel_job` with that ID. Direct SDK callers can use an `Idempotency-Key`
(maximum 128 visible ASCII bytes) and explicitly opt into one ambiguous
transport retry; the same key and canonical job spec then resolve to the
original job.

Tool results include both MCP structured content and the same JSON serialized
as text for older hosts. Execution failures such as a nonzero exit, timeout,
OOM kill, or policy violation are successful tool transport results with a
terminal Rookhold status; connection, authentication, validation, and adapter
policy failures use MCP `isError: true`.

Terminal MCP results also carry `attestation` status metadata from the job
detail: availability, the bound tenant, key ID, content digests and sizes,
media type, and the two tenant-scoped download paths. The adapter deliberately
does not embed the DSSE envelope or result artifact in MCP output, and it does
not label signatures as verified. A trusted host can download the exact files
through an SDK, retain them with the parent trace, and invoke
`rookhold-verify verify` offline using an independently pinned operator public
key. Treat the public-key API as discovery, not trust bootstrap; a successful
verification still requires the host to provide its expected tenant and
evaluate the authenticated `outcome` and `event_chain_complete` fields.

## Production checklist

- run a reviewed provider on the dedicated Linux x86_64 VM described in
  [deployment](deployment.md)
- use a private or TLS Rookhold endpoint and an integration-specific tenant key
- set `ROOKHOLD_MCP_MINIMUM_ISOLATION` to the exact workflow class (the guarded
  deployment uses `gvisor-application-kernel`); retain the legacy boolean only
  for an older configuration that intentionally means `linux-shared-kernel`
- restrict `ROOKHOLD_MCP_ALLOWED_LANGUAGES` and adapter ceilings
- set the MCP host timeout above the intended Rookhold wait budget
- disable alternate execution tools when Rookhold is mandatory
- run `rookhold check`, then run a canary through the actual harness
- retain the job ID in the parent agent trace and inspect the terminal
  `isolation_class`, receipt, and attestation metadata; download and verify the
  exact signed files when portable evidence is required
