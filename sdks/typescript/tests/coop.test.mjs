import assert from "node:assert/strict";
import test from "node:test";

import { Coop, CoopError } from "../dist/coop.js";

function response(value, status = 200, headers = {}) {
  return new Response(value === undefined ? null : JSON.stringify(value), {
    status,
    headers: { "content-type": "application/json", ...headers },
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
  const coop = new Coop("https://example.test/prefix/", "secret", { fetch: transport.fetch });
  await coop.submit("python", "print(1)", { stdin: "x", limits: { mem_mb: 128 } });
  assert.equal(transport.calls[0].url.toString(), "https://example.test/prefix/v1/jobs");
  assert.deepEqual(JSON.parse(transport.calls[0].init.body), {
    language: "python",
    code: "print(1)",
    stdin: "x",
    limits: { mem_mb: 128 },
  });
});

test("structured error preserves server diagnostics and retry delay", async () => {
  const transport = queuedFetch(response({ error: { code: "queue_full", message: "busy", request_id: "req-1", retryable: true } }, 503, { "retry-after": "2" }));
  const coop = new Coop("https://example.test", "secret", { fetch: transport.fetch });
  await assert.rejects(coop.jobs(), (error) => {
    assert(error instanceof CoopError);
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
  const coop = new Coop("https://example.test", "secret", { fetch: transport.fetch });
  await assert.rejects(coop.result("missing", 1_000), (error) => {
    assert(error instanceof CoopError);
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
  );
  const result = await new Coop("https://example.test", "secret", {
    fetch: transport.fetch,
  }).result("j", 1_000);
  assert.equal(result.stdout, "ok");
  assert.equal(transport.calls.length, 3);
});

test("wait and result deadlines reject non-finite values and zero is immediate", async () => {
  let calls = 0;
  const fetch = async () => {
    calls += 1;
    return response({ status: "running" });
  };
  const coop = new Coop("https://example.test", "secret", { fetch });
  await assert.rejects(coop.wait("j", 0), (error) => {
    assert(error instanceof CoopError);
    assert.equal(error.code, "job_wait_timeout");
    return true;
  });
  await assert.rejects(coop.result("j", 0), (error) => {
    assert(error instanceof CoopError);
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
  const coop = new Coop("https://example.test", "secret", { fetch: transport.fetch });
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
  const coop = new Coop("https://example.test", "secret", { fetch: transport.fetch });
  assert.deepEqual((await coop.replay("j", 1)).map((event) => event.seq), [2]);
});

test("legacy minus-one cursor is normalized for v0.2 servers", async () => {
  const transport = queuedFetch(response([]));
  const coop = new Coop("https://example.test", "secret", { fetch: transport.fetch });
  await coop.eventPage("j", { after: -1 });
  assert.equal(transport.calls[0].url.searchParams.get("after"), "0");
});

test("replay follows every cursor page", async () => {
  const transport = queuedFetch(
    response({ events: [{ seq: 1 }], next_cursor: 1 }),
    response({ events: [{ seq: 2 }], next_cursor: null }),
  );
  const coop = new Coop("https://example.test", "secret", { fetch: transport.fetch });
  assert.deepEqual((await coop.replay("j")).map((event) => event.seq), [1, 2]);
  assert.equal(transport.calls[1].url.searchParams.get("after"), "1");
});

test("whoami and capabilities expose the v0.2 discovery endpoints", async () => {
  const transport = queuedFetch(
    response({ tenant: "acme" }),
    response({
      version: "0.2.0",
      languages: ["python"],
      execution: { backend: "gvisor", isolated: true },
      limits: { wall_seconds_max: 300 },
      features: { stream_tickets: true },
    }),
  );
  const coop = new Coop("https://example.test", "secret", { fetch: transport.fetch });
  assert.equal((await coop.whoami()).tenant, "acme");
  assert.equal((await coop.capabilities()).features.stream_tickets, true);
  assert.equal(transport.calls[0].url.pathname, "/v1/whoami");
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
      bootstrap_ready: null,
      isolated: null,
      seccomp: null,
      network_allowed: null,
      networking: null,
      private_rootfs: null,
      dedicated_bootstrap: null,
      limit_enforcement: null,
    },
    receipt: null,
  };
  const known = {
    ...base,
    job_id: "done",
    status: "succeeded",
    effective_spec: effectiveSpec,
    execution_policy: {
      sandbox: "namespaces+cgroups-v2+private-rootfs",
      bootstrap_ready: true,
      isolated: true,
      seccomp: true,
      network_allowed: false,
      networking: "disabled",
      private_rootfs: true,
      dedicated_bootstrap: true,
      limit_enforcement: enforcement,
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
      backend: "namespaces+cgroups-v2+private-rootfs",
      bootstrap_ready: true,
      isolated: true,
      seccomp: true,
      network_allowed: false,
      networking: "disabled",
      private_rootfs: true,
      dedicated_bootstrap: true,
      effective_limits: effectiveSpec.limits,
      limit_enforcement: enforcement,
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
  const coop = new Coop("https://example.test", "secret", { fetch: transport.fetch });

  const queued = await coop.get("queued");
  const complete = await coop.get("done");
  const restarted = await coop.get("recovered");

  assert.equal(queued.effective_spec, null);
  assert.equal(queued.execution_policy.sandbox, null);
  assert.equal(
    complete.receipt.output.encoding,
    "utf8-event-lines-joined-by-lf-no-trailing-lf",
  );
  assert.equal(complete.receipt.executor_output.stdout.raw_sha256, "e".repeat(64));
  assert.equal(complete.receipt.private_rootfs, true);
  assert.equal(complete.receipt.dedicated_bootstrap, true);
  assert.equal(complete.receipt.bootstrap_ready, true);
  assert.deepEqual(complete.receipt.limit_enforcement, enforcement);
  assert.equal(restarted.effective_spec, null);
  assert.equal(restarted.execution_policy.bootstrap_ready, null);
  assert.equal("output" in restarted.receipt, false);
  assert.equal("private_rootfs" in restarted.receipt, false);
  assert.equal("dedicated_bootstrap" in restarted.receipt, false);
  assert.deepEqual(restarted.receipt.requested_limits, { wall_seconds: 15 });
});

test("request timeout becomes a retryable CoopError", async () => {
  const fetch = (_url, init) => new Promise((_resolve, reject) => {
    init.signal.addEventListener("abort", () => reject(new Error("aborted")), { once: true });
  });
  const coop = new Coop("https://example.test", "secret", { fetch, timeoutMs: 5 });
  await assert.rejects(coop.get("j"), (error) => {
    assert(error instanceof CoopError);
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
  const coop = new Coop("https://example.test", "secret", { fetch });
  const pending = coop.get("j", { signal: controller.signal });
  controller.abort();
  await assert.rejects(pending, (error) => {
    assert(error instanceof CoopError);
    assert.equal(error.code, "request_aborted");
    assert.equal(error.retryable, false);
    return true;
  });
});

test("WebSocket streaming uses a one-use ticket and de-duplicates cursor replay", async () => {
  const transport = queuedFetch(response({ ticket: "one use", stream_url: "/v1/jobs/j/stream", expires_at_ms: 1 }));
  const socket = new FakeSocket();
  let socketUrl;
  const coop = new Coop("https://example.test/prefix", "secret", {
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
  const coop = new Coop("https://example.test", "secret", {
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
  const defaultClient = new Coop("https://example.test", "secret", {
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

  const optedInTransport = queuedFetch(response("not found", 404));
  const optedInSocket = new FakeSocket();
  let optedInUrl;
  const optedInClient = new Coop("https://example.test", "secret", {
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
    (error) => error instanceof CoopError && error.code === "legacy_query_key_opt_in_required",
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
  const coop = new Coop("https://example.test", "secret", { fetch: transport.fetch });
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
  const coop = new Coop("https://example.test", "secret", { fetch });
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
  const coop = new Coop("https://example.test", "secret", { fetch });
  const events = [];
  for await (const event of coop.streamEvents("j", {
    preferWebSocket: false,
    pollIntervalMs: 1,
  })) events.push(event);
  assert.deepEqual(events.map((event) => event.kind), ["finished"]);
  assert.equal(replays, 6);
  assert.equal(statusRequests, 1);
});

test("base URL rejects credentials and query confusion", () => {
  assert.throws(() => new Coop("ftp://example.test", "secret"));
  assert.throws(() => new Coop("https://user@example.test", "secret"));
  assert.throws(() => new Coop("https://example.test?q=1", "secret"));
});
