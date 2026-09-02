# SDKs

Rookhold ships small reference clients under `sdks/`. They intentionally mirror the HTTP API and are suitable for embedding in agent tool loops. The OpenAPI document remains the canonical contract for generated clients.

After v0.8.0 publishes, install the exact release assets:

```bash
pip install https://github.com/sambai-dev/rookhold/releases/download/v0.8.0/rookhold-0.8.0-py3-none-any.whl
npm install https://github.com/sambai-dev/rookhold/releases/download/v0.8.0/rookhold-0.8.0.tgz
```

Named PyPI and npm installs are deferred while maintainer registry accounts are
activated. A protected follow-up workflow will publish these same immutable
v0.8.0 bytes through registry trusted publishing; it does not rebuild them.

Before publication, install from the checkout with
`python -m pip install ./sdks/python` or `npm install ./sdks/typescript`.

Exact candidate release files:

- [Python wheel](https://github.com/sambai-dev/rookhold/releases/download/v0.8.0/rookhold-0.8.0-py3-none-any.whl)
- [Python source distribution](https://github.com/sambai-dev/rookhold/releases/download/v0.8.0/rookhold-0.8.0.tar.gz)
- [npm tarball](https://github.com/sambai-dev/rookhold/releases/download/v0.8.0/rookhold-0.8.0.tgz)
- [Combined SPDX SBOM](https://github.com/sambai-dev/rookhold/releases/download/v0.8.0/rookhold-0.8.0.spdx.json)
- [SHA256SUMS](https://github.com/sambai-dev/rookhold/releases/download/v0.8.0/SHA256SUMS)

To install the v0.8.0 release, activate the intended Python virtual environment and download the exact release assets into an otherwise empty working directory. This example verifies both the checksum manifest and GitHub provenance before installation:

```bash
set -euo pipefail
version=0.8.0
python_asset="rookhold-${version}-py3-none-any.whl"
typescript_asset="rookhold-${version}.tgz"
sdk_asset_dir="$PWD"
gh release download "v${version}" --repo sambai-dev/rookhold \
  --pattern "$python_asset" \
  --pattern "$typescript_asset" \
  --pattern SHA256SUMS
verify_github_asset() {
  gh release verify-asset "v${version}" "$1" --repo sambai-dev/rookhold
  gh attestation verify "$1" \
    --repo sambai-dev/rookhold \
    --signer-workflow sambai-dev/rookhold/.github/workflows/release.yml \
    --source-ref "refs/tags/v${version}" \
    --predicate-type https://slsa.dev/provenance/v1 \
    --deny-self-hosted-runners
}
verify_github_asset SHA256SUMS
for asset in "$python_asset" "$typescript_asset"; do
  expected=$(awk -v file="$asset" '
    $2 == file && $1 ~ /^[0-9a-f]{64}$/ { digest=$1; count++ }
    END { if (count != 1) exit 1; print digest }
  ' SHA256SUMS)
  printf '%s  %s\n' "$expected" "$asset" | sha256sum --check --strict -
  verify_github_asset "$asset"
done
python -m pip install --no-deps "./$python_asset"
# Run this last command from the consuming Node.js project:
npm install "${sdk_asset_dir}/${typescript_asset}"
```

The release also includes `rookhold-0.8.0.tar.gz` for consumers that require a Python source distribution. On macOS or Windows, use the platform checksum commands shown in [deployment](deployment.md) in place of `sha256sum`; keep the same release-asset and constrained workflow-provenance verification.

## Python

The Python client uses only the standard library. From a source checkout, install it with `python -m pip install --no-deps ./sdks/python`, then:

The same package installs `rookhold-cli` for human terminal use and
`rookhold-mcp` for Claude Code, OpenCode, Codex, and other MCP hosts. The CLI
uses the typed client below; its `/mcp` command initializes the real adapter and
shows the live capability-narrowed tool list.

```python
from rookhold import Limits, Rookhold

client = Rookhold("http://127.0.0.1:7300", "rookhold-dev-key")
job = client.submit(
    "python",
    "print(6 * 7)",
    limits=Limits(wall_seconds=10, mem_mb=256),
)
result = client.result(job["job_id"], timeout=60)
print(result["stdout"])
```

Pass resource fields inside the `limits` object. The client accepts a default transport timeout and per-call deadlines; close/cancel work when the upstream agent request is abandoned.

Both SDKs accept `requirements.minimum_isolation` using the server's exact
isolation-class strings. `submit_result()` / `submitResult()` preserve the
ordinary submission body while exposing `Location` and
`Idempotency-Replayed`. Idempotency keys are limited to 128 visible ASCII
bytes; ambiguous retries are explicit opt-ins and always reuse the same key.

In both clients, `get` and `wait` return a job detail rather than the sparse
submission shape. The requested spec is complete, with nullable stored `stdin`
and all requested limits. `EffectiveJobSpec` uses `EffectiveLimits`, whose
individual values can be null when the backend did not enforce a control or
never reached its ready boundary. Its nullable `isolation_class` is observed
execution evidence, not a copy of capability configuration.
`LimitEnforcement` provides an explicit
boolean for each resource control. In development subprocess mode, only wall
time is effective; CPU, memory, process, and file values are null. The whole
effective spec and execution-policy fields are nullable for queued, migrated,
or restart-recovered rows without execution evidence. Typed receipt,
event-chain, durable output, executor-output, and resource schemas are
exported. Recovery receipts intentionally omit evidence that could not be
reconstructed after a restart, so those execution-specific members remain
optional; requested limits may also be partial on migrated records.

`JobDetail.attestation` reports signed-evidence availability, the tenant bound
into that evidence, immutable digests, sizes, media type, key ID, and
tenant-scoped relative download URLs.
`download_attestation` / `downloadAttestation` and
`download_result_artifact` / `downloadResultArtifact` return exact bytes with
content type and length. They reject a missing, malformed, or mismatched
`X-Content-Sha256` and never decode then re-encode JSON. This validates HTTP
content integrity only. It does not verify DSSE or establish trust in the
advertised key.

Save both byte arrays directly and verify them offline with an independently
pinned public key:

```bash
rookhold-verify verify \
  --envelope job.dsse.json \
  --subject job-result.json \
  --public-key trusted-rookhold-attestation.pub.pem \
  --tenant "$EXPECTED_TENANT" \
  --subject-name "coop://jobs/$JOB_ID/result" \
  --media-type application/vnd.coop.execution-result.v1+json
```

Use a tenant expected by the surrounding workflow (normally checked against
the authenticated `JobDetail.tenant` or `whoami` response), rather than copying
an untrusted tenant out of the downloaded JSON. The exact result and signed
predicate both carry that tenant; migrated v0.3 receipts may still omit it.

The typed `attestation_public_key()` / `attestationPublicKey()` endpoint is
useful for discovery and rotation diagnostics, but its own `trust_notice` is
literal: authenticate and pin the key out of band. A successful verifier exit
authenticates the claim and exact subject, not a successful execution outcome.

## TypeScript

Build and test the TypeScript package with `npm ci && npm test && npm run typecheck` under `sdks/typescript`; `npm pack` then creates an installable tarball for a consuming project. It uses platform `fetch` and `WebSocket`:

```ts
import { Rookhold } from "rookhold";

const client = new Rookhold("http://127.0.0.1:7300", "rookhold-dev-key");
const job = await client.submit("node", "console.log(6 * 7)", {
  limits: { wall_seconds: 10, mem_mb: 256 },
});
const result = await client.result(job.job_id, 60_000);
console.log(result.stdout);
```

Browser clients must normally be served from the same origin as Rookhold. The
server does not enable permissive CORS. Cross-origin browser access requires an
explicit origin allowlist at the reverse proxy, and exposes the bearer key to
the frontend runtime.

Node versions without a global `WebSocket` fall back to cursor replay. Both SDKs mint a one-use stream ticket before opening a WebSocket. Legacy API-key query compatibility is disabled by default and accepts only an explicit opt-in plus an unstructured v0.1 endpoint-missing response; structured Rookhold errors never put a key in a URL. Review [api.md](api.md) before enabling it for a trusted legacy server.

`wait` and `result` reject non-finite deadlines, treat a zero budget as an immediate timeout, and cap each in-flight request to the remaining overall budget. A structured `job_not_found` response is returned to the caller and is never mistaken for evidence that `/result` or `/stream-ticket` is a missing legacy route.

The server may deliberately close a response connection after 30 seconds with zero socket write progress, and every connection (including an upgraded WebSocket) has a 10-minute absolute lifetime. Both clients treat an early EOF, a rejected response body, or a `Content-Length` mismatch as a retryable transport failure rather than parsing partial JSON. It is safe to retry detail, result, and replay reads with bounded backoff. Persist a replay cursor only after the whole page has decoded successfully; after a failure, request the previous cursor again and deduplicate events by `(job_id, seq)`. A WebSocket reconnect mints a new one-use ticket and resumes from the last accepted sequence. Submission is different: repeat an ambiguously acknowledged `POST /v1/jobs` only when it carried an `Idempotency-Key`, and reuse the exact key and canonical job specification.

## Integration pattern

Expose one narrow tool to the model:

1. validate the requested language and your application-level policy;
2. submit to Rookhold with limits smaller than or equal to your tenant ceiling;
3. wait through `/result` or consume the stream;
4. return bounded stdout, stderr, terminal status, and violations to the agent;
5. retain the Rookhold job ID in the parent trace for investigation.

Do not give a model the Rookhold API key or let it choose the Rookhold base URL. Keep
both in the trusted tool adapter. Configure the adapter's minimum isolation as
operator policy and validate the terminal observed class. Do not transparently
retry an unkeyed submission because a network timeout does not prove the server
rejected the first request.

## Compatibility

The clients follow the additive v0.4 routes while retaining documented legacy
fallbacks. Python continues to export `Coop` and `CoopError` from `coop`; the
TypeScript package provides the same aliases from `rookhold/coop`. The v1
`coop://` subject and `application/vnd.coop...` media type in verification
examples are intentional evidence identities. Before upgrading either side
independently, run its tests against the target server and compare
`/openapi.json`. Rookhold does not yet promise a multi-major compatibility
window.
