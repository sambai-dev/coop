# SDKs

Coop ships small reference clients under `sdks/`. They intentionally mirror the HTTP API and are suitable for embedding in agent tool loops. The OpenAPI document remains the canonical contract for generated clients.

The v0.3 release workflow tests SDK source and installs both the built Python wheel and source distribution before including them, plus the npm package tarball, in the checksummed, attested GitHub release. It does not publish PyPI or npm registries. Use an exact GitHub release asset or the source-checkout paths below until a separately authenticated registry release is announced.

To install the v0.3.0 release, activate the intended Python virtual environment and download the exact release assets into an otherwise empty working directory. This example verifies both the checksum manifest and GitHub provenance before installation:

```bash
version=0.3.0
python_asset="coop_sdk-${version}-py3-none-any.whl"
typescript_asset="coop-sdk-${version}.tgz"
sdk_asset_dir="$PWD"
gh release download "v${version}" --repo sambai-dev/coop \
  --pattern "$python_asset" \
  --pattern "$typescript_asset" \
  --pattern SHA256SUMS
sha256sum --check --ignore-missing SHA256SUMS
gh attestation verify "$python_asset" --repo sambai-dev/coop
gh attestation verify "$typescript_asset" --repo sambai-dev/coop
python -m pip install --no-deps "./$python_asset"
# Run this last command from the consuming Node.js project:
npm install "${sdk_asset_dir}/${typescript_asset}"
```

The release also includes `coop_sdk-0.3.0.tar.gz` for consumers that require a Python source distribution. On macOS or Windows, use the platform checksum commands shown in [deployment](deployment.md) in place of `sha256sum`; the asset names and `gh attestation verify` commands are unchanged.

## Python

The Python client uses only the standard library. From a source checkout, install it with `python -m pip install --no-deps ./sdks/python`, then:

```python
from coop import Coop, Limits

client = Coop("http://127.0.0.1:7300", "coop-dev-key")
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

## TypeScript

Build and test the TypeScript package with `npm ci && npm test && npm run typecheck` under `sdks/typescript`; `npm pack` then creates an installable tarball for a consuming project. It uses platform `fetch` and `WebSocket`:

```ts
import { Coop } from "coop-sdk";

const client = new Coop("http://127.0.0.1:7300", "coop-dev-key");
const job = await client.submit("node", "console.log(6 * 7)", {
  limits: { wall_seconds: 10, mem_mb: 256 },
});
const result = await client.result(job.job_id, 60_000);
console.log(result.stdout);
```

Browser clients must normally be served from the same origin as Coop. The
server does not enable permissive CORS. Cross-origin browser access requires an
explicit origin allowlist at the reverse proxy, and exposes the bearer key to
the frontend runtime.

Node versions without a global `WebSocket` fall back to cursor replay. Both SDKs mint a one-use stream ticket before opening a WebSocket. Legacy API-key query compatibility is disabled by default and accepts only an explicit opt-in plus an unstructured v0.1 endpoint-missing response; structured v0.2 errors never put a key in a URL. Review [api.md](api.md) before enabling it for a trusted legacy server.

`wait` and `result` reject non-finite deadlines, treat a zero budget as an immediate timeout, and cap each in-flight request to the remaining overall budget. A structured v0.2 `job_not_found` response is returned to the caller and is never mistaken for evidence that `/result` or `/stream-ticket` is a missing legacy route.

The server may deliberately close a response connection after 30 seconds with zero socket write progress, and every connection (including an upgraded WebSocket) has a 10-minute absolute lifetime. Both clients treat an early EOF, a rejected response body, or a `Content-Length` mismatch as a retryable transport failure rather than parsing partial JSON. It is safe to retry detail, result, and replay reads with bounded backoff. Persist a replay cursor only after the whole page has decoded successfully; after a failure, request the previous cursor again and deduplicate events by `(job_id, seq)`. A WebSocket reconnect mints a new one-use ticket and resumes from the last accepted sequence. Submission is different: repeat an ambiguously acknowledged `POST /v1/jobs` only when it carried an `Idempotency-Key`, and reuse the exact key and canonical job specification.

## Integration pattern

Expose one narrow tool to the model:

1. validate the requested language and your application-level policy;
2. submit to Coop with limits smaller than or equal to your tenant ceiling;
3. wait through `/result` or consume the stream;
4. return bounded stdout, stderr, terminal status, and violations to the agent;
5. retain the Coop job ID in the parent trace for investigation.

Do not give a model the Coop API key or let it choose the Coop base URL. Keep
both in the trusted tool adapter. Configure the adapter's minimum isolation as
operator policy and validate the terminal observed class. Do not transparently
retry an unkeyed submission because a network timeout does not prove the server
rejected the first request.

## Compatibility

The clients follow the v0.2 routes. Before upgrading either side independently, run its tests against the target server and compare `/openapi.json`. Coop does not yet promise a multi-major compatibility window.
