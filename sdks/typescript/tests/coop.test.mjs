import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import test from "node:test";

import {
  Rookhold,
  RookholdError,
  ISOLATION_CLASSES,
  isolationSatisfies,
} from "../dist/rookhold.js";
import { Coop, CoopError } from "../dist/coop.js";

test("legacy Coop exports alias the Rookhold client", () => {
  assert.equal(Coop, Rookhold);
  assert.equal(CoopError, RookholdError);
});

function response(value, status = 200, headers = {}) {
  return new Response(value === undefined ? null : JSON.stringify(value), {
    status,
    headers: { "content-type": "application/json", ...headers },
  });
}

function binaryResponse(content, contentType, headers = {}) {
  const bytes = Uint8Array.from(content);
  return new Response(bytes, {
    status: 200,
    headers: {
      "content-type": contentType,
      "content-length": String(bytes.byteLength),
      "x-content-sha256": createHash("sha256").update(bytes).digest("hex"),
      ...headers,
    },
  });
}

function queuedFetch(...responses) {
  const calls = [];
  const queue = [...responses];
  const fetch = async (url, init) => {
    calls.push({ url: new URL(url), init });
    const next = queue.shift();
    if (next instanceof Error) throw next;
    return next;
  };
  return { calls, fetch };
}

class FakeSocket {
  readyState = 1;
  listeners = new Map();
  closed = false;

  addEventListener(type, listener) {
    const listeners = this.listeners.get(type) ?? [];
    listeners.push(listener);
    this.listeners.set(type, listeners);
  }

  removeEventListener(type, listener) {
    this.listeners.set(type, (this.listeners.get(type) ?? []).filter((item) => item !== listener));
  }

  close() {
    this.closed = true;
  }

  emit(type, event) {
    for (const listener of this.listeners.get(type) ?? []) listener(event);
  }
}

test("submit sends one correctly nested limits object", async () => {
  const transport = queuedFetch(response({ job_id: "j", status: "queued", stream_url: "/s", replay_url: "/r" }, 201));
  const coop = new Rookhold("https://example.test/prefix/", "secret", { fetch: transport.fetch });
  await coop.submit("python", "print(1)", { stdin: "x", limits: { mem_mb: 128 } });
  assert.equal(transport.calls[0].url.toString(), "https://example.test/prefix/v1/jobs");
  assert.deepEqual(JSON.parse(transport.calls[0].init.body), {
    language: "python",
    code: "print(1)",
    stdin: "x",
    limits: { mem_mb: 128 },
  });
});

test("submit sends a typed minimum-isolation requirement", async () => {
  const transport = queuedFetch(
    response({ job_id: "j", status: "queued", stream_url: "/s", replay_url: "/r" }, 201),
  );
  const coop = new Rookhold("https://example.test", "secret", { fetch: transport.fetch });
  await coop.submit("python", "print(1)", {
    requirements: { minimum_isolation: "linux-shared-kernel" },
  });
  assert.deepEqual(JSON.parse(transport.calls[0].init.body).requirements, {
    minimum_isolation: "linux-shared-kernel",
  });
});

test("isolation satisfaction matches the server's branched ordering", () => {
  assert.deepEqual(ISOLATION_CLASSES, [
    "none",
    "linux-shared-kernel",
    "gvisor-application-kernel",
    "wasm-capability",
    "hardware-vm",
    "confidential-vm",
  ]);
  assert.equal(isolationSatisfies("gvisor-application-kernel", "linux-shared-kernel"), true);
  assert.equal(isolationSatisfies("confidential-vm", "hardware-vm"), true);
  assert.equal(isolationSatisfies("linux-shared-kernel", "gvisor-application-kernel"), false);
  assert.equal(isolationSatisfies("wasm-capability", "wasm-capability"), true);
  assert.equal(isolationSatisfies("hardware-vm", "wasm-capability"), false);
  assert.equal(isolationSatisfies("wasm-capability", "linux-shared-kernel"), false);
  assert.equal(isolationSatisfies("future-provider", "linux-shared-kernel"), false);
  assert.equal(isolationSatisfies("hardware-vm", "future-minimum"), false);
  for (const actual of ISOLATION_CLASSES) {
    assert.equal(isolationSatisfies(actual, "none"), true);
  }
});

test("submitResult exposes Location and Idempotency-Replayed headers additively", async () => {
  const body = {
    job_id: "j",
    status: "queued",
    stream_url: "/v1/jobs/j/stream",
    replay_url: "/v1/jobs/j/replay",
    stream_ticket_url: "/v1/jobs/j/stream-ticket",
  };
  const transport = queuedFetch(
    response(body, 201, {
      Location: "/v1/jobs/j",
      "Idempotency-Replayed": "false",
    }),
    response(body, 201, {
      Location: "/v1/jobs/j",
      "Idempotency-Replayed": "true",
    }),
  );
  const coop = new Rookhold("https://example.test", "secret", { fetch: transport.fetch });
  assert.deepEqual(await coop.submitResult("python", "pass", {
    idempotencyKey: "logical-request-result",
  }), {
    job: body,
    location: "/v1/jobs/j",
    idempotency_replayed: false,
  });
  assert.deepEqual(await coop.submitResult("python", "pass", {
    idempotencyKey: "logical-request-result",
  }), {
    job: body,
    location: "/v1/jobs/j",
    idempotency_replayed: true,
  });
});

test("submit preserves its body-only compatibility contract", async () => {
  const body = { job_id: "j", status: "queued", stream_url: "/s", replay_url: "/r" };
  const transport = queuedFetch(response(body, 201, {
    Location: "/v1/jobs/j",
    "Idempotency-Replayed": "true",
  }));
  const coop = new Rookhold("https://example.test", "secret", { fetch: transport.fetch });
  assert.deepEqual(await coop.submit("python", "pass"), body);
});

test("submitResult rejects an invalid Idempotency-Replayed header", async () => {
  const transport = queuedFetch(response(
    { job_id: "j", status: "queued", stream_url: "/s", replay_url: "/r" },
    201,
    { "Idempotency-Replayed": "yes" },
  ));
  const coop = new Rookhold("https://example.test", "secret", { fetch: transport.fetch });
  await assert.rejects(
    coop.submitResult("python", "pass", { idempotencyKey: "logical-request-invalid-header" }),
    (error) => error instanceof RookholdError && error.code === "invalid_response",
  );
});

test("ambiguous submission retry reuses one generated idempotency key", async () => {
  const transport = queuedFetch(
    new Error("connection reset after write"),
    response({ job_id: "j", status: "queued", stream_url: "/s", replay_url: "/r" }, 201),
  );
  const coop = new Rookhold("https://example.test", "secret", { fetch: transport.fetch });
  const submitted = await coop.submit("python", "print(1)", { retryAmbiguous: true });
  assert.equal(submitted.job_id, "j");
  assert.equal(transport.calls.length, 2);
  const firstKey = transport.calls[0].init.headers["Idempotency-Key"];
  const secondKey = transport.calls[1].init.headers["Idempotency-Key"];
  assert.match(firstKey, /^[!-~]{1,128}$/);
  assert.equal(secondKey, firstKey);
});

test("submission ambiguity is not retryable without an idempotency key", async () => {
  const unkeyed = queuedFetch(new Error("connection reset"));
  const coop = new Rookhold("https://example.test", "secret", { fetch: unkeyed.fetch });
  await assert.rejects(coop.submit("python", "print(1)"), (error) => {
    assert(error instanceof RookholdError);
    assert.equal(error.code, "transport_error");
    assert.equal(error.retryable, false);
    assert.equal(error.idempotencyKey, undefined);
    return true;
  });

  const keyed = queuedFetch(new Error("connection reset"));
  const keyedClient = new Rookhold("https://example.test", "secret", { fetch: keyed.fetch });
  await assert.rejects(
    keyedClient.submit("python", "print(1)", { idempotencyKey: "logical-request-1" }),
    (error) => {
      assert(error instanceof RookholdError);
      assert.equal(error.retryable, true);
      assert.equal(error.idempotencyKey, "logical-request-1");
      return true;
    },
  );
  assert.equal(keyed.calls[0].init.headers["Idempotency-Key"], "logical-request-1");
});

test("ambiguous retry never repeats a structured HTTP rejection", async () => {
  const transport = queuedFetch(
    response({ error: { code: "queue_full", message: "busy", retryable: true } }, 503),
    response({ job_id: "should-not-run" }, 201),
  );
  const coop = new Rookhold("https://example.test", "secret", { fetch: transport.fetch });
  await assert.rejects(
    coop.submit("python", "print(1)", {
      idempotencyKey: "logical-request-2",
      retryAmbiguous: true,
    }),
    (error) => error instanceof RookholdError && error.code === "queue_full",
  );
  assert.equal(transport.calls.length, 1);
});

test("submission idempotency keys reject controls, whitespace, and overlength", async () => {
  let calls = 0;
  const coop = new Rookhold("https://example.test", "secret", {
    fetch: async () => {
      calls += 1;
      return response({});
    },
  });
  await coop.submit("python", "", { idempotencyKey: "x".repeat(128) });
  await assert.rejects(coop.submit("python", "", { idempotencyKey: "contains space" }), TypeError);
  await assert.rejects(coop.submit("python", "", { idempotencyKey: "line\nbreak" }), TypeError);
  await assert.rejects(coop.submit("python", "", { idempotencyKey: "x".repeat(129) }), TypeError);
  assert.equal(calls, 1);
});

test("ambiguous retry does not repeat a client-side serialization failure", async () => {
  let calls = 0;
  const cyclic = {};
  cyclic.self = cyclic;
  const coop = new Rookhold("https://example.test", "secret", {
    fetch: async () => {
      calls += 1;
      return response({});
    },
  });
  await assert.rejects(
    coop.submit("python", "", {
      limits: cyclic,
      idempotencyKey: "logical-request-cyclic",
      retryAmbiguous: true,
    }),
    (error) => {
      assert(error instanceof RookholdError);
      assert.equal(error.code, "invalid_request");
      assert.equal(error.retryable, false);
      assert.equal(error.idempotencyKey, "logical-request-cyclic");
      return true;
    },
  );
  assert.equal(calls, 0);
});

test("cancelResult returns the typed v0.4 cancellation outcome", async () => {
  const cancellation = {
    job: {
      job_id: "j",
      tenant: "acme",
      language: "python",
      status: "running",
      created_at_ms: 1,
      started_at_ms: 2,
      finished_at_ms: null,
      exit_code: null,
    },
    cancellation_requested: true,
    already_terminal: false,
  };
  const transport = queuedFetch(response(cancellation));
  const coop = new Rookhold("https://example.test", "secret", { fetch: transport.fetch });
  assert.deepEqual(await coop.cancelResult("j"), cancellation);
  assert.equal(transport.calls.length, 1);
  assert.equal(transport.calls[0].init.method, "DELETE");
});

test("cancelResult adapts a legacy empty 200 without another request", async () => {
  const transport = queuedFetch(response(undefined));
  const coop = new Rookhold("https://example.test", "secret", { fetch: transport.fetch });
  assert.deepEqual(await coop.cancelResult("j"), {
    job: null,
    cancellation_requested: true,
    already_terminal: false,
  });
  assert.equal(transport.calls.length, 1);
});

test("cancel remains a void compatibility wrapper", async () => {
  const transport = queuedFetch(response({
    job: { job_id: "j" },
    cancellation_requested: false,
    already_terminal: true,
  }));
  const coop = new Rookhold("https://example.test", "secret", { fetch: transport.fetch });
  assert.equal(await coop.cancel("j"), undefined);
});

test("structured error preserves server diagnostics and retry delay", async () => {
  const transport = queuedFetch(response({ error: { code: "queue_full", message: "busy", request_id: "req-1", retryable: true } }, 503, { "retry-after": "2" }));
  const coop = new Rookhold("https://example.test", "secret", { fetch: transport.fetch });
  await assert.rejects(coop.jobs(), (error) => {
    assert(error instanceof RookholdError);
    assert.equal(error.status, 503);
    assert.equal(error.code, "queue_full");
    assert.equal(error.requestId, "req-1");
    assert.equal(error.retryAfterMs, 2_000);
    assert.equal(error.retryable, true);
    return true;
  });
});

test("structured result 404 is not treated as a legacy route", async () => {
  const transport = queuedFetch(response({
    error: {
      code: "job_not_found",
      message: "job does not exist",
      request_id: "req-result",
      retryable: false,
    },
  }, 404));
  const coop = new Rookhold("https://example.test", "secret", { fetch: transport.fetch });
  await assert.rejects(coop.result("missing", 1_000), (error) => {
    assert(error instanceof RookholdError);
    assert.equal(error.code, "job_not_found");
    return true;
  });
  assert.equal(transport.calls.length, 1);
});

test("unstructured result 404 uses the legacy polling fallback", async () => {
  const transport = queuedFetch(
    response("not found", 404),
    response({
      job_id: "j",
      tenant: "acme",
      language: "python",
      status: "succeeded",
      created_at_ms: 1,
      started_at_ms: 2,
      finished_at_ms: 3,
      exit_code: 0,
    }),
    response({
      events: [{ seq: 1, ts_ms: 2, kind: "stdout", data: { line: "ok" } }],
      next_cursor: null,
    }),
    response({ events: [], next_cursor: null }),
    response({ events: [], next_cursor: null }),
  );
  const result = await new Rookhold("https://example.test", "secret", {
    fetch: transport.fetch,
  }).result("j", 1_000);
  assert.equal(result.stdout, "ok");
  assert.equal(transport.calls.length, 5);
});

test("legacy result polling retries an empty terminal replay before folding", async () => {
  const transport = queuedFetch(
    response("not found", 404),
    response({
      job_id: "j",
      tenant: "acme",
      language: "python",
      status: "succeeded",
      created_at_ms: 1,
      started_at_ms: 2,
      finished_at_ms: 3,
      exit_code: 0,
    }),
    response({ events: [], next_cursor: null }),
    response({
      events: [
        { seq: 1, ts_ms: 2, kind: "stdout", data: { line: "late" } },
        { seq: 2, ts_ms: 3, kind: "finished", data: { status: "succeeded" } },
      ],
      next_cursor: null,
    }),
  );
  const result = await new Rookhold("https://example.test", "secret", {
    fetch: transport.fetch,
  }).result("j", 1_000);
  assert.equal(result.stdout, "late");
  assert.equal(transport.calls.length, 4);
});

test("wait and result deadlines reject non-finite values and zero is immediate", async () => {
  let calls = 0;
  const fetch = async () => {
    calls += 1;
    return response({ status: "running" });
  };
  const coop = new Rookhold("https://example.test", "secret", { fetch });
  await assert.rejects(coop.wait("j", 0), (error) => {
    assert(error instanceof RookholdError);
    assert.equal(error.code, "job_wait_timeout");
    return true;
  });
  await assert.rejects(coop.result("j", 0), (error) => {
    assert(error instanceof RookholdError);
    assert.equal(error.code, "job_wait_timeout");
    return true;
  });
  await assert.rejects(coop.wait("j", Number.NaN), RangeError);
  await assert.rejects(coop.result("j", Number.POSITIVE_INFINITY), RangeError);
  assert.equal(calls, 0);
});

test("list and replay use the v0.2 cursor envelopes", async () => {
  const transport = queuedFetch(
    response({ items: [{ job_id: "j", status: "running" }], next_cursor: "opaque" }),
    response({ events: [{ seq: 4, kind: "stdout", data: { line: "x" }, ts_ms: 1 }], next_cursor: 4 }),
  );
  const coop = new Rookhold("https://example.test", "secret", { fetch: transport.fetch });
  const jobs = await coop.list({ limit: 10, cursor: "before", status: "running", language: "python" });
  const events = await coop.eventPage("job/id", { after: 3, limit: 20 });
  assert.equal(jobs.next_cursor, "opaque");
  assert.equal(events.next_cursor, 4);
  assert.equal(transport.calls[0].url.searchParams.get("cursor"), "before");
  assert.match(transport.calls[1].url.pathname, /job%2Fid/);
  assert.equal(transport.calls[1].url.searchParams.get("after"), "3");
});

test("legacy array replay filters events after the requested sequence", async () => {
  const transport = queuedFetch(response([{ seq: 1 }, { seq: 2 }]));
  const coop = new Rookhold("https://example.test", "secret", { fetch: transport.fetch });
  assert.deepEqual((await coop.replay("j", 1)).map((event) => event.seq), [2]);
});

test("legacy minus-one cursor is normalized for v0.2 servers", async () => {
  const transport = queuedFetch(response([]));
  const coop = new Rookhold("https://example.test", "secret", { fetch: transport.fetch });
  await coop.eventPage("j", { after: -1 });
  assert.equal(transport.calls[0].url.searchParams.get("after"), "0");
});

test("replay follows every cursor page", async () => {
  const transport = queuedFetch(
    response({ events: [{ seq: 1 }], next_cursor: 1 }),
    response({ events: [{ seq: 2 }], next_cursor: null }),
  );
  const coop = new Rookhold("https://example.test", "secret", { fetch: transport.fetch });
  assert.deepEqual((await coop.replay("j")).map((event) => event.seq), [1, 2]);
  assert.equal(transport.calls[1].url.searchParams.get("after"), "1");
});

test("replay rejects a non-advancing initial cursor", async () => {
  const transport = queuedFetch(response({ events: [], next_cursor: 0 }));
  const coop = new Rookhold("https://example.test", "secret", { fetch: transport.fetch });
  await assert.rejects(
    coop.replay("j"),
    (error) => error instanceof RookholdError && error.code === "invalid_response",
  );
  assert.equal(transport.calls.length, 1);
});

test("whoami and capabilities expose the current discovery contract", async () => {
  const enforcement = {
    wall_seconds: true,
    cpu_seconds: true,
    mem_mb: true,
    max_pids: true,
    max_file_mb: true,
  };
  const transport = queuedFetch(
    response({
      tenant: "acme",
      principal_id: "legacy:acme",
      credential_id: null,
      auth_method: "api_key",
      scopes: ["jobs:submit", "jobs:read", "jobs:cancel", "service:read", "metrics:read"],
      expires_at_ms: null,
    }),
    response({
      version: "0.7.0",
      languages: ["python"],
      execution: {
        backend: "gvisor",
        isolation_class: "gvisor-application-kernel",
        isolated: true,
        private_rootfs: true,
        dedicated_bootstrap: true,
        seccomp: false,
        networking: "disabled",
        limit_enforcement: enforcement,
      },
      limits: {
        wall_seconds_max: 300,
        cpu_seconds_max: 240,
        mem_mb_max: 1_024,
        concurrent_mem_mb_max: 4_096,
        pids_max: 1_024,
        file_mb_max: 512,
        output_lines_max: 10_000,
        output_bytes_per_stream_max: 1_048_576,
        output_record_bytes_max: 16_384,
        code_bytes_max: 1_048_576,
        stdin_bytes_max: 1_048_576,
      },
      features: {
        result_wait: true,
        cancellation: true,
        event_cursors: true,
        stream_tickets: true,
        receipts: true,
        signed_attestations: true,
      },
      attestations: {
        enabled: true,
        algorithm: "Ed25519",
        envelope_format: "DSSE/in-toto Statement v1",
        key_id: "ed25519:abc",
        public_key_url: "/v1/attestation/public-key",
      },
    }),
    response({
      algorithm: "Ed25519",
      key_id: "ed25519:abc",
      public_key_pem: "-----BEGIN PUBLIC KEY-----\nabc\n-----END PUBLIC KEY-----\n",
      trust_notice: "Pin this key out of band.",
    }),
  );
  const coop = new Rookhold("https://example.test", "secret", { fetch: transport.fetch });
  const identity = await coop.whoami();
  const capabilities = await coop.capabilities();
  const publicKey = await coop.attestationPublicKey();
  assert.equal(identity.principal_id, "legacy:acme");
  assert.deepEqual(identity.scopes, [
    "jobs:submit",
    "jobs:read",
    "jobs:cancel",
    "service:read",
    "metrics:read",
  ]);
  assert.equal(capabilities.execution.isolation_class, "gvisor-application-kernel");
  assert.equal(capabilities.limits.concurrent_mem_mb_max, 4_096);
  assert.equal(capabilities.features.stream_tickets, true);
  assert.equal(capabilities.features.signed_attestations, true);
  assert.equal(capabilities.attestations.algorithm, "Ed25519");
  assert.equal(publicKey.key_id, "ed25519:abc");
  assert.equal(transport.calls[2].init.headers.Authorization, "Bearer secret");
  assert.equal(transport.calls[0].url.pathname, "/v1/whoami");
});

test("attestation downloads preserve exact binary order and expose validated metadata", async () => {
  const envelopeBytes = Uint8Array.from([0, 255, 1, 128, 10, 13, 123, 125]);
  const artifactBytes = Uint8Array.from([125, 123, 13, 10, 128, 1, 255, 0]);
  const transport = queuedFetch(
    binaryResponse(envelopeBytes, "application/vnd.dsse.envelope.v1+json"),
    binaryResponse(artifactBytes, "application/vnd.coop.execution-result.v1+json"),
  );
  const coop = new Rookhold("https://example.test/prefix", "tenant-secret", {
    fetch: transport.fetch,
  });

  const envelope = await coop.downloadAttestation("job/one");
  const artifact = await coop.downloadResultArtifact("job/one");

  assert.deepEqual(Array.from(envelope.content), Array.from(envelopeBytes));
  assert.deepEqual(Array.from(artifact.content), Array.from(artifactBytes));
  assert.equal(envelope.contentLength, envelopeBytes.byteLength);
  assert.equal(envelope.contentType, "application/vnd.dsse.envelope.v1+json");
  assert.equal(
    envelope.sha256,
    createHash("sha256").update(envelopeBytes).digest("hex"),
  );
  assert.equal(transport.calls[0].url.pathname, "/prefix/v1/jobs/job%2Fone/attestation");
  assert.equal(
    transport.calls[1].url.pathname,
    "/prefix/v1/jobs/job%2Fone/result-artifact",
  );
  assert.equal(
    transport.calls[0].init.headers.Accept,
    "application/vnd.dsse.envelope.v1+json",
  );
  for (const call of transport.calls) {
    assert.equal(call.init.redirect, "error");
    assert.equal(call.init.headers.Authorization, "Bearer tenant-secret");
    assert.equal(call.url.toString().includes("tenant-secret"), false);
  }
});

test("artifact downloads reject missing, malformed, and mismatched digests", async () => {
  const content = Uint8Array.from([1, 2, 3]);
  const cases = [
    {
      headers: { "content-type": "application/octet-stream" },
      code: "invalid_response",
    },
    {
      headers: {
        "content-type": "application/octet-stream",
        "x-content-sha256": "not-a-digest",
      },
      code: "invalid_response",
    },
    {
      headers: {
        "content-type": "application/octet-stream",
        "x-content-sha256": "0".repeat(64),
      },
      code: "content_digest_mismatch",
    },
  ];
  for (const { headers, code } of cases) {
    const coop = new Rookhold("https://example.test", "secret", {
      fetch: async () => new Response(content, { status: 200, headers }),
    });
    await assert.rejects(
      coop.downloadAttestation("job"),
      (error) => error instanceof RookholdError && error.code === code,
    );
  }
});

test("artifact downloads preserve tenant-scoped structured 404s", async () => {
  const transport = queuedFetch(response({
    error: {
      code: "attestation_unavailable",
      message: "no signed attestation",
      request_id: "req-attest",
      retryable: false,
    },
  }, 404));
  const coop = new Rookhold("https://example.test", "secret", { fetch: transport.fetch });

  await assert.rejects(coop.downloadAttestation("foreign-or-missing"), (error) => {
    assert(error instanceof RookholdError);
    assert.equal(error.status, 404);
    assert.equal(error.code, "attestation_unavailable");
    assert.equal(error.requestId, "req-attest");
    assert.equal(error.retryable, false);
    return true;
  });
  assert.equal(transport.calls.length, 1);
  assert.equal(transport.calls[0].init.headers.Authorization, "Bearer secret");
});

test("artifact downloads reject redirected and cross-origin responses", async () => {
  const bytes = Uint8Array.from([1]);
  const redirected = binaryResponse(bytes, "application/octet-stream");
  Object.defineProperty(redirected, "redirected", { value: true });
  const crossOrigin = binaryResponse(bytes, "application/octet-stream");
  Object.defineProperty(crossOrigin, "url", { value: "https://attacker.example/evidence" });
  const transport = queuedFetch(redirected, crossOrigin);
  const coop = new Rookhold("https://example.test", "tenant-secret", { fetch: transport.fetch });

  await assert.rejects(
    coop.downloadAttestation("job"),
    (error) => error instanceof RookholdError && error.code === "unsafe_redirect" &&
      !String(error).includes("tenant-secret"),
  );
  await assert.rejects(
    coop.downloadResultArtifact("job"),
    (error) => error instanceof RookholdError && error.code === "unsafe_redirect" &&
      !String(error).includes("tenant-secret"),
  );
  assert.equal(transport.calls.every((call) => call.init.redirect === "error"), true);
});

test("artifact download deadlines and aborts stop active transfers", async () => {
  const hangingFetch = (_url, init) => new Promise((_resolve, reject) => {
    init.signal.addEventListener("abort", () => reject(new Error("aborted")), { once: true });
  });
  const coop = new Rookhold("https://example.test", "secret", { fetch: hangingFetch });

  await assert.rejects(coop.downloadAttestation("job", { timeoutMs: 5 }), (error) => {
    assert(error instanceof RookholdError);
    assert.equal(error.code, "request_timeout");
    assert.equal(error.retryable, true);
    return true;
  });

  const controller = new AbortController();
  const pending = coop.downloadResultArtifact("job", { signal: controller.signal });
  controller.abort();
  await assert.rejects(pending, (error) => {
    assert(error instanceof RookholdError);
    assert.equal(error.code, "request_aborted");
    assert.equal(error.retryable, false);
    return true;
  });
});

test("job detail distinguishes unknown policy from persisted and executor evidence", async () => {
  const storedSpec = {
    language: "python",
    code: "print('ok')",
    stdin: null,
    limits: {
      wall_seconds: 15,
      cpu_seconds: 10,
      mem_mb: 256,
      max_pids: 128,
      max_file_mb: 16,
      allow_network: false,
    },
    requirements: { minimum_isolation: "linux-shared-kernel" },
  };
  const enforcement = {
    wall_seconds: true,
    cpu_seconds: true,
    mem_mb: true,
    max_pids: true,
    max_file_mb: true,
  };
  const effectiveSpec = {
    ...storedSpec,
    limits: { ...storedSpec.limits, allow_network: false },
    isolation_class: "gvisor-application-kernel",
  };
  const base = {
    tenant: "acme",
    language: "python",
    created_at_ms: 1,
    started_at_ms: null,
    finished_at_ms: null,
    exit_code: null,
    requested_spec: storedSpec,
    receipt_sha256: null,
  };
  const unknown = {
    ...base,
    job_id: "queued",
    status: "queued",
    effective_spec: null,
    execution_policy: {
      sandbox: null,
      isolation_class: null,
      bootstrap_ready: null,
      isolated: null,
      seccomp: null,
      network_allowed: null,
      networking: null,
      private_rootfs: null,
      dedicated_bootstrap: null,
      limit_enforcement: null,
      runtime_version: null,
      runtime_sha256: null,
      rootfs_sha256: null,
      config_sha256: null,
    },
    receipt: null,
  };
  const known = {
    ...base,
    job_id: "done",
    status: "succeeded",
    effective_spec: effectiveSpec,
    receipt_sha256: "b".repeat(64),
    execution_policy: {
      sandbox: "gvisor",
      isolation_class: "gvisor-application-kernel",
      bootstrap_ready: true,
      isolated: true,
      seccomp: false,
      network_allowed: false,
      networking: "disabled",
      private_rootfs: true,
      dedicated_bootstrap: true,
      limit_enforcement: enforcement,
      runtime_version: "runsc version 20260817.0",
      runtime_sha256: "1".repeat(64),
      rootfs_sha256: "2".repeat(64),
      config_sha256: "3".repeat(64),
    },
    receipt: {
      version: 1,
      job_id: "done",
      outcome: "succeeded",
      exit_code: 0,
      finished_at_ms: 3,
      duration_ms: 1,
      event_chain: {
        version: 1,
        head: "a".repeat(64),
        events: 3,
        event_count: 3,
        verified_events: 3,
        legacy_events: 0,
        complete: true,
      },
      receipt_sha256: "b".repeat(64),
      backend: "gvisor",
      requirements: storedSpec.requirements,
      minimum_isolation: "linux-shared-kernel",
      isolation_class: "gvisor-application-kernel",
      bootstrap_ready: true,
      isolated: true,
      seccomp: false,
      network_allowed: false,
      networking: "disabled",
      private_rootfs: true,
      dedicated_bootstrap: true,
      effective_limits: effectiveSpec.limits,
      limit_enforcement: enforcement,
      runtime_version: "runsc version 20260817.0",
      runtime_sha256: "1".repeat(64),
      rootfs_sha256: "2".repeat(64),
      config_sha256: "3".repeat(64),
      output: {
        encoding: "utf8-event-lines-joined-by-lf-no-trailing-lf",
        stdout_bytes: 2,
        stderr_bytes: 0,
        stdout_sha256: "c".repeat(64),
        stderr_sha256: "d".repeat(64),
        truncated: false,
      },
      resource_usage: {
        wall_time_ms: 1,
        cpu_time_usec: 2,
        memory_peak_bytes: 3,
      },
      executor_output: {
        stdout: {
          bytes_seen: 2,
          bytes_offered_to_sink: 2,
          records_offered_to_sink: 1,
          raw_sha256: "e".repeat(64),
          executor_truncated: false,
        },
        stderr: {
          bytes_seen: 0,
          bytes_offered_to_sink: 0,
          records_offered_to_sink: 0,
          raw_sha256: "f".repeat(64),
          executor_truncated: false,
        },
      },
    },
  };
  const recovered = {
    ...base,
    job_id: "recovered",
    status: "error",
    effective_spec: null,
    execution_policy: unknown.execution_policy,
    receipt_sha256: "9".repeat(64),
    receipt: {
      version: 1,
      job_id: "recovered",
      outcome: "error",
      exit_code: null,
      finished_at_ms: 4,
      duration_ms: 2,
      terminal_reason: "server_restarted",
      requested_limits: { wall_seconds: 15 },
      event_chain: {
        version: 1,
        head: "8".repeat(64),
        events: 3,
        event_count: 3,
        verified_events: 3,
        legacy_events: 0,
        complete: true,
      },
      receipt_sha256: "9".repeat(64),
    },
  };
  const transport = queuedFetch(response(unknown), response(known), response(recovered));
  const coop = new Rookhold("https://example.test", "secret", { fetch: transport.fetch });

  const queued = await coop.get("queued");
  const complete = await coop.get("done");
  const restarted = await coop.get("recovered");

  assert.equal(queued.effective_spec, null);
  assert.equal(queued.execution_policy.sandbox, null);
  assert.equal(queued.execution_policy.isolation_class, null);
  assert.equal(
    complete.receipt.output.encoding,
    "utf8-event-lines-joined-by-lf-no-trailing-lf",
  );
  assert.equal(complete.receipt.executor_output.stdout.raw_sha256, "e".repeat(64));
  assert.equal(complete.receipt.private_rootfs, true);
  assert.equal(complete.receipt.dedicated_bootstrap, true);
  assert.equal(complete.receipt.bootstrap_ready, true);
  assert.equal(complete.effective_spec.isolation_class, "gvisor-application-kernel");
  assert.equal(complete.execution_policy.runtime_sha256, "1".repeat(64));
  assert.equal(complete.receipt.minimum_isolation, "linux-shared-kernel");
  assert.equal(complete.receipt.isolation_class, "gvisor-application-kernel");
  assert.equal(complete.receipt.config_sha256, "3".repeat(64));
  assert.deepEqual(complete.receipt.limit_enforcement, enforcement);
  assert.equal(restarted.effective_spec, null);
  assert.equal(restarted.execution_policy.bootstrap_ready, null);
  assert.equal("output" in restarted.receipt, false);
  assert.equal("private_rootfs" in restarted.receipt, false);
  assert.equal("dedicated_bootstrap" in restarted.receipt, false);
  assert.deepEqual(restarted.receipt.requested_limits, { wall_seconds: 15 });
});

test("request timeout becomes a retryable RookholdError", async () => {
  const fetch = (_url, init) => new Promise((_resolve, reject) => {
    init.signal.addEventListener("abort", () => reject(new Error("aborted")), { once: true });
  });
  const coop = new Rookhold("https://example.test", "secret", { fetch, timeoutMs: 5 });
  await assert.rejects(coop.get("j"), (error) => {
    assert(error instanceof RookholdError);
    assert.equal(error.code, "request_timeout");
    assert.equal(error.retryable, true);
    return true;
  });
});

test("an AbortController stops an active request", async () => {
  const fetch = (_url, init) => new Promise((_resolve, reject) => {
    init.signal.addEventListener("abort", () => reject(new Error("aborted")), { once: true });
  });
  const controller = new AbortController();
  const coop = new Rookhold("https://example.test", "secret", { fetch });
  const pending = coop.get("j", { signal: controller.signal });
  controller.abort();
  await assert.rejects(pending, (error) => {
    assert(error instanceof RookholdError);
    assert.equal(error.code, "request_aborted");
    assert.equal(error.retryable, false);
    return true;
  });
});

test("WebSocket streaming uses a one-use ticket and de-duplicates cursor replay", async () => {
  const transport = queuedFetch(
    response({ ticket: "one use", stream_url: "/v1/jobs/j/stream", expires_at_ms: 1 }),
    response({ events: [], next_cursor: null }),
  );
  const socket = new FakeSocket();
  let socketUrl;
  const coop = new Rookhold("https://example.test/prefix", "secret", {
    fetch: transport.fetch,
    webSocketFactory: (url) => {
      socketUrl = new URL(url);
      queueMicrotask(() => {
        socket.emit("message", { data: JSON.stringify({ seq: 2, ts_ms: 1, kind: "stdout", data: { line: "old" } }) });
        socket.emit("message", { data: JSON.stringify({ seq: 3, ts_ms: 2, kind: "stdout", data: { line: "new" } }) });
        socket.emit("message", { data: JSON.stringify({ seq: 4, ts_ms: 3, kind: "finished", data: { status: "succeeded" } }) });
      });
      return socket;
    },
  });
  const events = [];
  for await (const event of coop.streamEvents("j", { after: 2 })) events.push(event);
  assert.deepEqual(events.map((event) => event.seq), [3, 4]);
  assert.equal(socketUrl.searchParams.get("ticket"), "one use");
  assert.equal(socketUrl.searchParams.get("after"), "2");
  assert.equal(socketUrl.pathname, "/prefix/v1/jobs/j/stream");
  assert.equal(socket.closed, true);
});

test("WebSocket decoding preserves arrival order for asynchronous frame bodies", async () => {
  const transport = queuedFetch(
    response({ ticket: "ordered", stream_url: "/v1/jobs/j/stream", expires_at_ms: 1 }),
    response({ events: [], next_cursor: null }),
  );
  const socket = new FakeSocket();
  const coop = new Rookhold("https://example.test", "secret", {
    fetch: transport.fetch,
    webSocketFactory: () => {
      queueMicrotask(() => {
        socket.emit("message", {
          data: {
            text: () => new Promise((resolve) => setTimeout(() => resolve(JSON.stringify({
              seq: 1,
              ts_ms: 1,
              kind: "stdout",
              data: { line: "first" },
            })), 10)),
          },
        });
        socket.emit("message", {
          data: {
            text: () => Promise.resolve(JSON.stringify({
              seq: 2,
              ts_ms: 2,
              kind: "finished",
              data: { status: "succeeded" },
            })),
          },
        });
      });
      return socket;
    },
  });
  const events = [];
  for await (const event of coop.streamEvents("j")) events.push(event.seq);
  assert.deepEqual(events, [1, 2]);
});

test("terminal WebSocket event performs one durable tail replay", async () => {
  const transport = queuedFetch(
    response({ ticket: "catch-up", stream_url: "/v1/jobs/j/stream", expires_at_ms: 1 }),
    response({
      events: [{ seq: 2, ts_ms: 2, kind: "legacy_tail", data: {} }],
      next_cursor: 2,
    }),
  );
  const socket = new FakeSocket();
  const coop = new Rookhold("https://example.test", "secret", {
    fetch: transport.fetch,
    webSocketFactory: () => {
      queueMicrotask(() => socket.emit("message", {
        data: JSON.stringify({ seq: 1, ts_ms: 1, kind: "finished", data: { status: "succeeded" } }),
      }));
      return socket;
    },
  });
  const events = [];
  for await (const event of coop.streamEvents("j")) events.push(event.seq);
  assert.deepEqual(events, [1, 2]);
  assert.equal(transport.calls[1].url.searchParams.get("after"), "1");
});

test("stream stop fences already-buffered WebSocket callbacks", async () => {
  const transport = queuedFetch(
    response({ ticket: "stop", stream_url: "/v1/jobs/j/stream", expires_at_ms: 1 }),
  );
  const socket = new FakeSocket();
  const coop = new Rookhold("https://example.test", "secret", {
    fetch: transport.fetch,
    webSocketFactory: () => {
      queueMicrotask(() => {
        for (let seq = 1; seq <= 4; seq += 1) {
          socket.emit("message", {
            data: JSON.stringify({ seq, ts_ms: seq, kind: "stdout", data: { line: String(seq) } }),
          });
        }
      });
      return socket;
    },
  });
  const events = [];
  let stop = () => {};
  stop = coop.stream("j", (event) => {
    events.push(event.seq);
    stop();
  });
  await new Promise((resolve) => setTimeout(resolve, 10));
  assert.deepEqual(events, [1]);
  assert.equal(socket.closed, true);
});

test("structured v0.2 ticket 404 never places a key in a URL", async () => {
  const transport = queuedFetch(
    response({
      error: {
        code: "job_not_found",
        message: "job does not exist",
        request_id: "req-stream",
        retryable: false,
      },
    }, 404),
    response({
      events: [{ seq: 1, ts_ms: 1, kind: "finished", data: { status: "failed" } }],
      next_cursor: null,
    }),
  );
  const socketUrls = [];
  const coop = new Rookhold("https://example.test", "secret", {
    fetch: transport.fetch,
    webSocketFactory: (url) => {
      socketUrls.push(url);
      return new FakeSocket();
    },
  });
  const events = [];
  for await (const event of coop.streamEvents("missing", {
    allowLegacyQueryKey: true,
    pollIntervalMs: 1,
  })) events.push(event);
  assert.deepEqual(events.map((event) => event.kind), ["finished"]);
  assert.deepEqual(socketUrls, []);
  assert(transport.calls.every((call) => !call.url.searchParams.has("key")));
});

test("legacy query-key streaming requires explicit opt-in", async () => {
  const defaultTransport = queuedFetch(
    response("not found", 404),
    response({
      events: [{ seq: 1, ts_ms: 1, kind: "finished", data: { status: "failed" } }],
      next_cursor: null,
    }),
  );
  const defaultSocketUrls = [];
  const defaultClient = new Rookhold("https://example.test", "secret", {
    fetch: defaultTransport.fetch,
    webSocketFactory: (url) => {
      defaultSocketUrls.push(url);
      return new FakeSocket();
    },
  });
  for await (const _event of defaultClient.streamEvents("j", { pollIntervalMs: 1 })) {
    // Drain the terminal replay event.
  }
  assert.deepEqual(defaultSocketUrls, []);

  const optedInTransport = queuedFetch(
    response("not found", 404),
    response({ events: [], next_cursor: null }),
  );
  const optedInSocket = new FakeSocket();
  let optedInUrl;
  const optedInClient = new Rookhold("https://example.test", "secret", {
    fetch: optedInTransport.fetch,
    webSocketFactory: (url) => {
      optedInUrl = new URL(url);
      queueMicrotask(() => {
        optedInSocket.emit("message", {
          data: JSON.stringify({
            seq: 1,
            ts_ms: 1,
            kind: "finished",
            data: { status: "failed" },
          }),
        });
      });
      return optedInSocket;
    },
  });
  for await (const _event of optedInClient.streamEvents("j", {
    allowLegacyQueryKey: true,
  })) {
    // Drain the terminal WebSocket event.
  }
  assert.equal(optedInUrl.searchParams.get("key"), "secret");

  assert.throws(
    () => optedInClient.stream("j", () => {}, "secret"),
    (error) => error instanceof RookholdError && error.code === "legacy_query_key_opt_in_required",
  );
});

test("polling stream yields the whole terminal replay page", async () => {
  const transport = queuedFetch(response({
    events: [
      { seq: 1, ts_ms: 1, kind: "finished", data: { status: "succeeded" } },
      { seq: 2, ts_ms: 2, kind: "legacy_tail", data: {} },
    ],
    next_cursor: null,
  }));
  const coop = new Rookhold("https://example.test", "secret", { fetch: transport.fetch });
  const events = [];
  for await (const event of coop.streamEvents("j", {
    preferWebSocket: false,
    pollIntervalMs: 1,
  })) events.push(event);
  assert.deepEqual(events.map((event) => event.seq), [1, 2]);
});

test("polling stream drains backlog before checking terminal status", async () => {
  let statusRequests = 0;
  const fetch = async (input) => {
    const url = new URL(input);
    if (url.pathname.endsWith("/replay")) {
      const after = Number(url.searchParams.get("after") ?? 0);
      const last = Math.min(3_002, after + 500);
      const events = [];
      for (let seq = after + 1; seq <= last; seq += 1) {
        events.push({
          seq,
          ts_ms: seq,
          kind: seq === 3_002 ? "finished" : "stdout",
          data: seq === 3_002 ? { status: "succeeded" } : { line: String(seq) },
        });
      }
      return response({
        events,
        next_cursor: events.at(-1)?.seq ?? null,
      });
    }
    statusRequests += 1;
    return response({ job_id: "j", status: "succeeded" });
  };
  const coop = new Rookhold("https://example.test", "secret", { fetch });
  const events = [];
  for await (const event of coop.streamEvents("j", {
    preferWebSocket: false,
    pollIntervalMs: 1,
  })) events.push(event);
  assert.equal(events.length, 3_002);
  assert.equal(events.at(-1).kind, "finished");
  assert.equal(statusRequests, 0);
});

test("terminal status is followed by a final catch-up replay", async () => {
  let replays = 0;
  let statusRequests = 0;
  const fetch = async (input) => {
    const url = new URL(input);
    if (url.pathname.endsWith("/replay")) {
      replays += 1;
      if (replays <= 5) return response({ events: [], next_cursor: null });
      return response({
        events: [{
          seq: 1,
          ts_ms: 1,
          kind: "finished",
          data: { status: "succeeded" },
        }],
        next_cursor: 1,
      });
    }
    statusRequests += 1;
    return response({ job_id: "j", status: "succeeded" });
  };
  const coop = new Rookhold("https://example.test", "secret", { fetch });
  const events = [];
  for await (const event of coop.streamEvents("j", {
    preferWebSocket: false,
    pollIntervalMs: 1,
  })) events.push(event);
  assert.deepEqual(events.map((event) => event.kind), ["finished"]);
  assert.equal(replays, 6);
  assert.equal(statusRequests, 1);
});

test("an empty replay after terminal projection does not hide the terminal event", async () => {
  let replays = 0;
  let statusRequests = 0;
  const fetch = async (input) => {
    const url = new URL(input);
    if (url.pathname.endsWith("/replay")) {
      replays += 1;
      if (replays <= 6) return response({ events: [], next_cursor: null });
      return response({
        events: [{
          seq: 1,
          ts_ms: 1,
          kind: "finished",
          data: { status: "succeeded" },
        }],
        next_cursor: 1,
      });
    }
    statusRequests += 1;
    return response({ job_id: "j", status: "succeeded" });
  };
  const coop = new Rookhold("https://example.test", "secret", { fetch });
  const events = [];
  for await (const event of coop.streamEvents("j", {
    preferWebSocket: false,
    pollIntervalMs: 1,
  })) events.push(event);
  assert.deepEqual(events.map((event) => event.kind), ["finished"]);
  assert.equal(replays, 7);
  assert.equal(statusRequests, 1);
});

test("polling abort fences the remainder of a buffered replay page", async () => {
  const transport = queuedFetch(response({
    events: [
      { seq: 1, ts_ms: 1, kind: "stdout", data: { line: "one" } },
      { seq: 2, ts_ms: 2, kind: "stdout", data: { line: "two" } },
      { seq: 3, ts_ms: 3, kind: "finished", data: { status: "succeeded" } },
    ],
    next_cursor: 3,
  }));
  const controller = new AbortController();
  const coop = new Rookhold("https://example.test", "secret", { fetch: transport.fetch });
  const events = [];
  await assert.rejects(async () => {
    for await (const event of coop.streamEvents("j", {
      preferWebSocket: false,
      signal: controller.signal,
    })) {
      events.push(event.seq);
      controller.abort();
    }
  }, (error) => error instanceof RookholdError && error.code === "request_aborted");
  assert.deepEqual(events, [1]);
});

test("polling stream rejects a full page that makes no cursor progress", async () => {
  let calls = 0;
  const duplicatePage = {
    events: Array.from({ length: 500 }, () => ({
      seq: 1,
      ts_ms: 1,
      kind: "stdout",
      data: { line: "duplicate" },
    })),
    next_cursor: 1,
  };
  const coop = new Rookhold("https://example.test", "secret", {
    fetch: async () => {
      calls += 1;
      return response(duplicatePage);
    },
  });
  await assert.rejects(async () => {
    for await (const _event of coop.streamEvents("j", {
      after: 1,
      preferWebSocket: false,
      pollIntervalMs: 60_000,
    })) {
      // The duplicate page must not yield or spin.
    }
  }, (error) => error instanceof RookholdError && error.code === "invalid_response");
  assert.equal(calls, 1);
});

test("client serialization and object spread do not expose the API key", () => {
  const coop = new Rookhold("https://example.test", "super-secret", {
    fetch: async () => response({}),
  });
  assert.equal(coop.apiKey, "super-secret");
  assert.equal(Object.keys(coop).includes("apiKey"), false);
  assert.equal(JSON.stringify(coop).includes("super-secret"), false);
  assert.equal(JSON.stringify({ ...coop }).includes("super-secret"), false);
});

test("base URL rejects credentials and query confusion", () => {
  assert.throws(() => new Rookhold("ftp://example.test", "secret"));
  assert.throws(() => new Rookhold("https://user@example.test", "secret"));
  assert.throws(() => new Rookhold("https://example.test?q=1", "secret"));
});
