/** Typed browser and Node.js client for the Coop execution API. */

export const JOB_STATUSES = [
  "queued",
  "running",
  "succeeded",
  "failed",
  "timed_out",
  "oom_killed",
  "cancelled",
  "error",
] as const;

export type JobStatus = (typeof JOB_STATUSES)[number];
export type Language = "python" | "node" | "bash";
export type EventKind =
  | "accepted"
  | "started"
  | "stdout"
  | "stderr"
  | "resource"
  | "violation"
  | "truncated"
  | "finished";

export interface Limits {
  wall_seconds?: number;
  cpu_seconds?: number;
  mem_mb?: number;
  max_pids?: number;
  max_file_mb?: number;
  allow_network?: boolean;
}

export interface JobSpec {
  language: Language | (string & {});
  code: string;
  stdin?: string;
  limits?: Limits;
}

/** Complete limits recorded after the server applies defaults or policy. */
export interface StoredLimits {
  wall_seconds: number;
  cpu_seconds: number;
  mem_mb: number;
  max_pids: number;
  max_file_mb: number;
  allow_network: boolean;
}

/** Complete requested spec returned by a job lookup. */
export interface StoredJobSpec {
  language: Language | (string & {});
  code: string;
  stdin: string | null;
  limits: StoredLimits;
}

/** Controls actually enforced for an execution; null means not enforced. */
export interface EffectiveLimits {
  wall_seconds: number | null;
  cpu_seconds: number | null;
  mem_mb: number | null;
  max_pids: number | null;
  max_file_mb: number | null;
  /** Observed network posture; null when the workload never became ready. */
  allow_network: boolean | null;
}

export interface EffectiveJobSpec {
  language: Language | (string & {});
  code: string;
  stdin: string | null;
  limits: EffectiveLimits;
}

export interface LimitEnforcement {
  wall_seconds: boolean;
  cpu_seconds: boolean;
  mem_mb: boolean;
  max_pids: boolean;
  max_file_mb: boolean;
}

export interface SubmitResponse {
  job_id: string;
  status: JobStatus;
  stream_url: string;
  replay_url: string;
  stream_ticket_url?: string;
}

export interface JobView {
  job_id: string;
  tenant: string;
  language: Language | (string & {});
  status: JobStatus;
  created_at_ms: number;
  started_at_ms: number | null;
  finished_at_ms: number | null;
  exit_code: number | null;
}

export interface ExecutionPolicy {
  /** Values are null for queued or migrated rows without execution evidence. */
  sandbox: string | null;
  bootstrap_ready: boolean | null;
  isolated: boolean | null;
  seccomp: boolean | null;
  network_allowed: boolean | null;
  networking: "disabled" | "host" | null;
  private_rootfs: boolean | null;
  dedicated_bootstrap: boolean | null;
  limit_enforcement: LimitEnforcement | null;
}

export interface JobDetail extends JobView {
  requested_spec: StoredJobSpec;
  effective_spec: EffectiveJobSpec | null;
  execution_policy: ExecutionPolicy;
  receipt: Receipt | null;
  receipt_sha256: string | null;
}

export type Job = JobView | JobDetail;

export interface CoopEvent {
  seq: number;
  ts_ms: number;
  kind: EventKind | (string & {});
  data: Record<string, unknown>;
  /** Absent or null on migrated v0.1 events. */
  prev_hash?: string | null;
  /** Absent or null on migrated v0.1 events. */
  event_hash?: string | null;
  /** Absent, null, or 0 on migrated v0.1 events. */
  hash_version?: number | null;
}

/** A v0.2 event after the caller has validated its evidence fields. */
export interface HashedCoopEvent extends CoopEvent {
  prev_hash: string | null;
  event_hash: string;
  hash_version: 1;
}

export interface EventChainReceipt {
  version: number;
  head: string | null;
  events: number;
  event_count: number;
  verified_events: number;
  legacy_events: number;
  complete: boolean;
}

export interface OutputEvidence {
  encoding: "utf8-event-lines-joined-by-lf-no-trailing-lf";
  stdout_bytes: number;
  stderr_bytes: number;
  stdout_sha256: string;
  stderr_sha256: string;
  truncated: boolean;
}

export interface ExecutorStreamEvidence {
  bytes_seen: number;
  bytes_offered_to_sink: number;
  records_offered_to_sink: number;
  raw_sha256: string;
  executor_truncated: boolean;
}

export interface ExecutorOutputEvidence {
  stdout: ExecutorStreamEvidence;
  stderr: ExecutorStreamEvidence;
}

export interface ResourceUsage {
  wall_time_ms: number;
  cpu_time_usec: number | null;
  memory_peak_bytes: number | null;
}

/** Limits retained as evidence; migrated/recovery receipts can be partial. */
export interface ReceiptLimits {
  wall_seconds?: number;
  cpu_seconds?: number;
  mem_mb?: number;
  max_pids?: number;
  max_file_mb?: number;
  allow_network?: boolean;
}

export interface Receipt {
  version: number;
  job_id: string;
  outcome: JobStatus;
  exit_code: number | null;
  finished_at_ms: number;
  duration_ms: number;
  event_chain: EventChainReceipt;
  receipt_sha256: string;
  /** Present when startup recovery finalized an interrupted run. */
  terminal_reason?: string;
  /** Execution-specific fields are absent on startup-recovery receipts. */
  killed_by?: string | null;
  created_at_ms?: number;
  started_at_ms?: number | null;
  backend?: string;
  bootstrap_ready?: boolean;
  isolated?: boolean;
  seccomp?: boolean;
  network_allowed?: boolean | null;
  networking?: "disabled" | "host" | null;
  private_rootfs?: boolean;
  dedicated_bootstrap?: boolean;
  evidence_complete?: boolean;
  requested_limits?: ReceiptLimits;
  effective_limits?: EffectiveLimits;
  limit_enforcement?: LimitEnforcement;
  code_sha256?: string;
  stdin_sha256?: string;
  policy_sha256?: string;
  resource_usage?: ResourceUsage | null;
  executor_output?: ExecutorOutputEvidence | null;
  output?: OutputEvidence;
}

export interface JobResult {
  job_id: string;
  status: JobStatus;
  exit_code: number | null;
  duration_ms: number | null;
  stdout: string;
  stderr: string;
  truncated: boolean;
  violations: Record<string, unknown>[];
}

export interface JobPage {
  items: JobView[];
  next_cursor: string | null;
}

export interface EventPage {
  events: CoopEvent[];
  next_cursor: number | null;
}

export interface StreamTicket {
  ticket: string;
  stream_url: string;
  expires_at_ms: number;
}

export interface Capabilities {
  version: string;
  languages: string[];
  execution: {
    backend: string;
    isolated: boolean;
    private_rootfs: boolean;
    dedicated_bootstrap: boolean;
    seccomp: boolean;
    networking: "disabled" | "host";
    limit_enforcement: LimitEnforcement;
  };
  limits: {
    wall_seconds_max: number;
    cpu_seconds_max: number;
    mem_mb_max: number;
    pids_max: number;
    file_mb_max: number;
    output_lines_max: number;
    output_bytes_per_stream_max: number;
    output_record_bytes_max: number;
    code_bytes_max: number;
    stdin_bytes_max: number;
  };
  features: {
    result_wait: boolean;
    cancellation: boolean;
    event_cursors: boolean;
    stream_tickets: boolean;
    receipts: boolean;
  };
}

export interface WhoAmI {
  tenant: string;
}

export interface RequestOptions {
  signal?: AbortSignal | undefined;
  timeoutMs?: number | undefined;
}

export interface SubmitOptions extends RequestOptions {
  stdin?: string | undefined;
  limits?: Limits | undefined;
}

export interface ListOptions extends RequestOptions {
  limit?: number | undefined;
  cursor?: string | undefined;
  status?: JobStatus | undefined;
  language?: Language | (string & {}) | undefined;
}

export interface ReplayOptions extends RequestOptions {
  after?: number | undefined;
  limit?: number | undefined;
}

export interface StreamOptions {
  after?: number | undefined;
  signal?: AbortSignal | undefined;
  preferWebSocket?: boolean | undefined;
  allowLegacyQueryKey?: boolean | undefined;
  /** Only for a v0.1 compatibility URL. Defaults to the client's API key. */
  legacyApiKey?: string | undefined;
  pollIntervalMs?: number | undefined;
  onError?: ((error: unknown) => void) | undefined;
}

export type FetchLike = (input: RequestInfo | URL, init?: RequestInit) => Promise<Response>;

export interface WebSocketLike {
  readonly readyState: number;
  close(code?: number, reason?: string): void;
  addEventListener?(type: string, listener: (event: unknown) => void): void;
  removeEventListener?(type: string, listener: (event: unknown) => void): void;
  onmessage?: ((event: any) => any) | null;
  onclose?: ((event: any) => any) | null;
  onerror?: ((event: any) => any) | null;
}

export type WebSocketFactory = (url: string) => WebSocketLike;

export interface ClientOptions {
  timeoutMs?: number | undefined;
  fetch?: FetchLike | undefined;
  webSocketFactory?: WebSocketFactory | undefined;
}

export interface CoopErrorInit {
  status?: number | undefined;
  code?: string | undefined;
  requestId?: string | undefined;
  retryable?: boolean | undefined;
  body?: string | undefined;
  retryAfterMs?: number | undefined;
  cause?: unknown;
}

export class CoopError extends Error {
  readonly status: number | undefined;
  readonly code: string;
  readonly requestId: string | undefined;
  readonly retryable: boolean;
  readonly body: string;
  readonly retryAfterMs: number | undefined;
  override readonly cause: unknown;

  constructor(message: string, init: CoopErrorInit = {}) {
    super(message);
    this.name = "CoopError";
    this.status = init.status;
    this.code = init.code ?? "unknown_error";
    this.requestId = init.requestId;
    this.retryable = init.retryable ?? false;
    this.body = init.body ?? "";
    this.retryAfterMs = init.retryAfterMs;
    this.cause = init.cause;
  }
}

const TERMINAL = new Set<JobStatus>(JOB_STATUSES.slice(2));

function isTerminal(status: unknown): status is JobStatus {
  return typeof status === "string" && TERMINAL.has(status as JobStatus);
}

function assertPositive(name: string, value: number): void {
  if (!Number.isFinite(value) || value <= 0) throw new RangeError(`${name} must be positive`);
}

function retryAfterMs(headers: Headers): number | undefined {
  const value = headers.get("retry-after");
  if (!value) return undefined;
  const seconds = Number(value);
  if (Number.isFinite(seconds)) return Math.max(0, seconds * 1_000);
  const timestamp = Date.parse(value);
  return Number.isFinite(timestamp) ? Math.max(0, timestamp - Date.now()) : undefined;
}

function parseHttpError(status: number, body: string, headers: Headers): CoopError {
  let code = `http_${status}`;
  let message = body.trim() || `HTTP ${status}`;
  let requestId = headers.get("x-request-id") ?? undefined;
  let retryable = status === 408 || status === 425 || status === 429 || status >= 500;
  try {
    const value = JSON.parse(body) as Record<string, unknown>;
    const nested = value.error;
    const error = nested && typeof nested === "object" ? (nested as Record<string, unknown>) : value;
    if (typeof error.code === "string") code = error.code;
    if (typeof error.message === "string") message = error.message;
    else if (typeof error.detail === "string") message = error.detail;
    if (typeof error.request_id === "string") requestId = error.request_id;
    else if (typeof value.request_id === "string") requestId = value.request_id;
    if (typeof error.retryable === "boolean") retryable = error.retryable;
  } catch {
    // Older servers return plain text; retain it as the error message.
  }
  return new CoopError(message, {
    status,
    code,
    requestId,
    retryable,
    body,
    retryAfterMs: retryAfterMs(headers),
  });
}

function abortError(message = "operation aborted"): CoopError {
  return new CoopError(message, { code: "request_aborted", retryable: false });
}

function delay(ms: number, signal?: AbortSignal): Promise<void> {
  if (signal?.aborted) return Promise.reject(abortError());
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      signal?.removeEventListener("abort", onAbort);
      resolve();
    }, ms);
    const onAbort = () => {
      clearTimeout(timer);
      reject(abortError());
    };
    signal?.addEventListener("abort", onAbort, { once: true });
  });
}

function eventData(event: unknown): unknown {
  if (event && typeof event === "object" && "data" in event) {
    return (event as { data: unknown }).data;
  }
  return event;
}

async function messageText(value: unknown): Promise<string> {
  if (typeof value === "string") return value;
  if (value instanceof ArrayBuffer) return new TextDecoder().decode(value);
  if (ArrayBuffer.isView(value)) {
    return new TextDecoder().decode(new Uint8Array(value.buffer, value.byteOffset, value.byteLength));
  }
  if (value && typeof value === "object" && "text" in value) {
    const text = (value as { text: () => Promise<string> }).text;
    if (typeof text === "function") return text.call(value);
  }
  return String(value);
}

interface RequestResult<T> {
  data: T;
  status: number;
}

export class Coop {
  readonly baseUrl: string;
  readonly apiKey: string;
  readonly timeoutMs: number;
  private readonly fetcher: FetchLike;
  private readonly webSocketFactory: WebSocketFactory | undefined;

  constructor(baseUrl: string, apiKey: string, options: ClientOptions = {}) {
    const parsed = new URL(baseUrl);
    if (parsed.protocol !== "http:" && parsed.protocol !== "https:") {
      throw new TypeError("baseUrl must use http: or https:");
    }
    if (parsed.username || parsed.password || parsed.search || parsed.hash) {
      throw new TypeError("baseUrl must not contain credentials, a query, or a fragment");
    }
    if (!apiKey.trim()) throw new TypeError("apiKey must not be empty");
    this.timeoutMs = options.timeoutMs ?? 30_000;
    assertPositive("timeoutMs", this.timeoutMs);
    parsed.pathname = parsed.pathname.replace(/\/+$/, "");
    this.baseUrl = parsed.toString().replace(/\/$/, "");
    this.apiKey = apiKey;

    const globalFetch = globalThis.fetch?.bind(globalThis) as FetchLike | undefined;
    const fetcher = options.fetch ?? globalFetch;
    if (!fetcher) throw new TypeError("a Fetch implementation is required");
    this.fetcher = fetcher;

    const GlobalWebSocket = globalThis.WebSocket;
    this.webSocketFactory =
      options.webSocketFactory ??
      (GlobalWebSocket ? (url: string) => new GlobalWebSocket(url) : undefined);
  }

  private url(path: string, query?: Record<string, string | number | undefined>): URL {
    const base = new URL(this.baseUrl);
    base.pathname = `${base.pathname.replace(/\/$/, "")}/${path.replace(/^\//, "")}`;
    base.search = "";
    for (const [key, value] of Object.entries(query ?? {})) {
      if (value !== undefined) base.searchParams.set(key, String(value));
    }
    return base;
  }

  private jobPath(jobId: string): string {
    if (!jobId) throw new TypeError("jobId must not be empty");
    return `/v1/jobs/${encodeURIComponent(jobId)}`;
  }

  private async requestResult<T>(
    method: string,
    path: string,
    body?: unknown,
    options: RequestOptions = {},
    query?: Record<string, string | number | undefined>,
  ): Promise<RequestResult<T>> {
    const timeoutMs = options.timeoutMs ?? this.timeoutMs;
    assertPositive("timeoutMs", timeoutMs);
    if (options.signal?.aborted) throw abortError();

    const controller = new AbortController();
    let timedOut = false;
    const onAbort = () => controller.abort(options.signal?.reason);
    options.signal?.addEventListener("abort", onAbort, { once: true });
    const timer = setTimeout(() => {
      timedOut = true;
      controller.abort();
    }, timeoutMs);

    try {
      let response: Response;
      try {
        response = await this.fetcher(this.url(path, query), {
          method,
          redirect: "error",
          signal: controller.signal,
          headers: {
            Accept: "application/json",
            Authorization: `Bearer ${this.apiKey}`,
            ...(body !== undefined ? { "Content-Type": "application/json" } : {}),
            "X-Coop-Client": "typescript/0.2.0",
          },
          ...(body !== undefined ? { body: JSON.stringify(body) } : {}),
        });
      } catch (cause) {
        if (timedOut) {
          throw new CoopError(`request timed out after ${timeoutMs}ms`, {
            code: "request_timeout",
            retryable: true,
            cause,
          });
        }
        if (options.signal?.aborted) throw abortError();
        throw new CoopError(cause instanceof Error ? cause.message : String(cause), {
          code: "transport_error",
          retryable: true,
          cause,
        });
      }

      let text: string;
      try {
        text = await response.text();
      } catch (cause) {
        if (timedOut) {
          throw new CoopError(`request timed out after ${timeoutMs}ms`, {
            code: "request_timeout",
            retryable: true,
            cause,
          });
        }
        if (options.signal?.aborted) throw abortError();
        throw new CoopError("failed to read the server response", {
          code: "transport_error",
          retryable: true,
          cause,
        });
      }
      if (!response.ok) throw parseHttpError(response.status, text, response.headers);
      if (!text) return { data: undefined as T, status: response.status };
      try {
        return { data: JSON.parse(text) as T, status: response.status };
      } catch (cause) {
        throw new CoopError("server returned invalid JSON", {
          status: response.status,
          code: "invalid_response",
          body: text,
          cause,
        });
      }
    } finally {
      clearTimeout(timer);
      options.signal?.removeEventListener("abort", onAbort);
    }
  }

  private async request<T>(
    method: string,
    path: string,
    body?: unknown,
    options: RequestOptions = {},
    query?: Record<string, string | number | undefined>,
  ): Promise<T> {
    return (await this.requestResult<T>(method, path, body, options, query)).data;
  }

  submit(
    language: Language | (string & {}),
    code: string,
    options: SubmitOptions = {},
  ): Promise<SubmitResponse> {
    const { signal, timeoutMs, stdin, limits } = options;
    const spec: JobSpec = { language, code };
    if (stdin !== undefined) spec.stdin = stdin;
    if (limits !== undefined) spec.limits = limits;
    return this.request("POST", "/v1/jobs", spec, { signal, timeoutMs });
  }

  get(jobId: string, options: RequestOptions = {}): Promise<JobDetail> {
    return this.request("GET", this.jobPath(jobId), undefined, options);
  }

  cancel(jobId: string, options: RequestOptions = {}): Promise<void> {
    return this.request("DELETE", this.jobPath(jobId), undefined, options);
  }

  whoami(options: RequestOptions = {}): Promise<WhoAmI> {
    return this.request("GET", "/v1/whoami", undefined, options);
  }

  capabilities(options: RequestOptions = {}): Promise<Capabilities> {
    return this.request("GET", "/v1/capabilities", undefined, options);
  }

  async list(options: ListOptions = {}): Promise<JobPage> {
    const limit = options.limit ?? 50;
    if (!Number.isInteger(limit) || limit < 1 || limit > 500) {
      throw new RangeError("limit must be an integer between 1 and 500");
    }
    const raw = await this.request<JobPage | JobView[]>(
      "GET",
      "/v1/jobs",
      undefined,
      options,
      {
        limit,
        cursor: options.cursor,
        status: options.status,
        language: options.language,
      },
    );
    if (Array.isArray(raw)) return { items: raw, next_cursor: null };
    if (!raw || !Array.isArray(raw.items)) {
      throw new CoopError("invalid job list envelope", { code: "invalid_response" });
    }
    return raw;
  }

  async jobs(limit = 50, options: RequestOptions = {}): Promise<JobView[]> {
    return (await this.list({ ...options, limit })).items;
  }

  async eventPage(jobId: string, options: ReplayOptions = {}): Promise<EventPage> {
    const after = options.after;
    const limit = options.limit ?? 500;
    if (after !== undefined && (!Number.isInteger(after) || after < -1)) {
      throw new RangeError("after must be -1 or a greater integer");
    }
    if (!Number.isInteger(limit) || limit < 1 || limit > 5_000) {
      throw new RangeError("limit must be an integer between 1 and 5000");
    }
    const raw = await this.request<EventPage | CoopEvent[]>(
      "GET",
      `${this.jobPath(jobId)}/replay`,
      undefined,
      options,
      { after: after === undefined ? undefined : Math.max(0, after), limit },
    );
    if (Array.isArray(raw)) {
      return {
        events: after === undefined ? raw : raw.filter((event) => event.seq > after),
        next_cursor: null,
      };
    }
    if (!raw || !Array.isArray(raw.events)) {
      throw new CoopError("invalid event replay envelope", { code: "invalid_response" });
    }
    return raw;
  }

  async replay(
    jobId: string,
    after?: number,
    limit = 1_000,
    options: RequestOptions = {},
  ): Promise<CoopEvent[]> {
    const events: CoopEvent[] = [];
    let cursor = after;
    while (true) {
      const page = await this.eventPage(jobId, { ...options, after: cursor, limit });
      events.push(...page.events);
      if (page.next_cursor === null) return events;
      if (cursor !== undefined && page.next_cursor <= cursor) {
        throw new CoopError("event cursor did not advance", { code: "invalid_response" });
      }
      cursor = page.next_cursor;
    }
  }

  async wait(
    jobId: string,
    timeoutMs = 60_000,
    pollMs = 1_000,
    signal?: AbortSignal,
  ): Promise<JobDetail> {
    if (!Number.isFinite(timeoutMs) || timeoutMs < 0) {
      throw new RangeError("timeoutMs must be finite and non-negative");
    }
    assertPositive("pollMs", pollMs);
    const deadline = Date.now() + timeoutMs;
    return this.waitUntil(jobId, deadline, timeoutMs, pollMs, signal);
  }

  private async waitUntil(
    jobId: string,
    deadline: number,
    timeoutLabelMs: number,
    pollMs: number,
    signal?: AbortSignal,
  ): Promise<JobDetail> {
    while (true) {
      const remaining = deadline - Date.now();
      if (remaining <= 0) {
        throw new CoopError(`job ${jobId} still running after ${timeoutLabelMs}ms`, {
          code: "job_wait_timeout",
          retryable: true,
        });
      }
      const view = await this.get(jobId, {
        signal,
        timeoutMs: remaining,
      });
      if (isTerminal(view.status)) return view;
      const sleepBudget = deadline - Date.now();
      if (sleepBudget <= 0) {
        throw new CoopError(`job ${jobId} still running after ${timeoutLabelMs}ms`, {
          code: "job_wait_timeout",
          retryable: true,
        });
      }
      await delay(Math.min(pollMs, sleepBudget), signal);
    }
  }

  async result(jobId: string, timeoutMs = 60_000, signal?: AbortSignal): Promise<JobResult> {
    if (!Number.isFinite(timeoutMs) || timeoutMs < 0) {
      throw new RangeError("timeoutMs must be finite and non-negative");
    }
    const deadline = Date.now() + timeoutMs;
    while (true) {
      const remaining = deadline - Date.now();
      if (remaining <= 0) {
        throw new CoopError(`job ${jobId} still running after ${timeoutMs}ms`, {
          code: "job_wait_timeout",
          retryable: true,
        });
      }
      const waitSeconds = Math.min(300, Math.floor(remaining / 1_000));
      try {
        const view = await this.request<JobResult>(
          "GET",
          `${this.jobPath(jobId)}/result`,
          undefined,
          { signal, timeoutMs: remaining },
          { wait_seconds: waitSeconds },
        );
        if (isTerminal(view.status)) return view;
      } catch (error) {
        if (
          !(error instanceof CoopError) ||
          (error.code !== "http_404" && error.code !== "http_405")
        ) {
          throw error;
        }
        return this.resultViaPolling(jobId, deadline, signal);
      }
      if (Date.now() >= deadline) {
        throw new CoopError(`job ${jobId} still running after ${timeoutMs}ms`, { code: "job_wait_timeout", retryable: true });
      }
      await delay(Math.min(1_000, Math.max(0, deadline - Date.now())), signal);
    }
  }

  private async resultViaPolling(jobId: string, deadline: number, signal?: AbortSignal): Promise<JobResult> {
    const waitBudget = deadline - Date.now();
    if (waitBudget <= 0) {
      throw new CoopError(`job ${jobId} result deadline expired`, {
        code: "job_wait_timeout",
        retryable: true,
      });
    }
    const view = await this.waitUntil(jobId, deadline, waitBudget, 1_000, signal);
    const stdout: string[] = [];
    const stderr: string[] = [];
    const violations: Record<string, unknown>[] = [];
    let truncated = false;
    let after: number | undefined;
    while (true) {
      const remaining = deadline - Date.now();
      if (remaining <= 0) {
        throw new CoopError(`job ${jobId} result deadline expired`, {
          code: "job_wait_timeout",
          retryable: true,
        });
      }
      const page = await this.eventPage(jobId, {
        after,
        signal,
        timeoutMs: remaining,
      });
      for (const event of page.events) {
        after = Math.max(after ?? -1, event.seq);
        const line = typeof event.data.line === "string" ? event.data.line : "";
        if (event.kind === "stdout") stdout.push(line);
        else if (event.kind === "stderr") stderr.push(line);
        else if (event.kind === "truncated") truncated = true;
        else if (event.kind === "violation") violations.push(event.data);
      }
      if (page.next_cursor === null) break;
      after = page.next_cursor;
    }
    return {
      job_id: jobId,
      status: view.status,
      exit_code: view.exit_code ?? null,
      duration_ms:
        view.started_at_ms != null && view.finished_at_ms != null
          ? view.finished_at_ms - view.started_at_ms
          : null,
      stdout: stdout.join("\n"),
      stderr: stderr.join("\n"),
      truncated,
      violations,
    };
  }

  private websocketUrl(value: string): URL {
    let url: URL;
    if (/^[a-z][a-z\d+.-]*:/i.test(value)) {
      url = new URL(value);
    } else {
      const relative = new URL(value, "http://relative.invalid");
      url = this.url(relative.pathname);
      url.search = relative.search;
    }
    if (url.protocol === "http:") url.protocol = "ws:";
    else if (url.protocol === "https:") url.protocol = "wss:";
    if (url.protocol !== "ws:" && url.protocol !== "wss:") {
      throw new CoopError("invalid stream URL", { code: "invalid_response" });
    }
    url.hash = "";
    return url;
  }

  private async streamUrl(jobId: string, after: number, options: StreamOptions): Promise<URL> {
    const path = this.jobPath(jobId);
    const wireAfter = Math.max(0, after);
    try {
      const ticket = await this.request<StreamTicket>(
        "POST",
        `${path}/stream-ticket`,
        undefined,
        { signal: options.signal },
      );
      if (!ticket || typeof ticket.ticket !== "string" || typeof ticket.stream_url !== "string") {
        throw new CoopError("invalid stream ticket response", { code: "invalid_response" });
      }
      const url = this.websocketUrl(ticket.stream_url);
      if (!url.searchParams.has("ticket")) url.searchParams.set("ticket", ticket.ticket);
      if (!url.searchParams.has("after")) url.searchParams.set("after", String(wireAfter));
      return url;
    } catch (error) {
      if (!(error instanceof CoopError) || (error.status !== 404 && error.status !== 405)) throw error;
      // A structured v0.2 `job_not_found` is not evidence that stream tickets
      // are unsupported. Query-key compatibility is explicit and limited to
      // the unstructured HTTP code emitted by legacy servers.
      const legacyEndpointMissing = error.code === "http_404" || error.code === "http_405";
      if (options.allowLegacyQueryKey !== true || !legacyEndpointMissing) {
        throw new CoopError("server does not support stream tickets and legacy key URLs are disabled", {
          code: "stream_ticket_unavailable",
          cause: error,
        });
      }
      const url = this.websocketUrl(`${path}/stream`);
      url.searchParams.set("key", options.legacyApiKey ?? this.apiKey);
      url.searchParams.set("after", String(wireAfter));
      return url;
    }
  }

  private async *socketEvents(url: URL, after: number, signal?: AbortSignal): AsyncGenerator<CoopEvent> {
    if (!this.webSocketFactory) return;
    if (signal?.aborted) throw abortError();
    const socket = this.webSocketFactory(url.toString());
    const queue: CoopEvent[] = [];
    let done = false;
    let failure: unknown;
    let wake: (() => void) | undefined;
    let cursor = after;
    const notify = () => {
      const current = wake;
      wake = undefined;
      current?.();
    };
    const onMessage = (message: unknown) => {
      void messageText(eventData(message))
        .then((text) => {
          const event = JSON.parse(text) as CoopEvent;
          if (typeof event.seq !== "number" || typeof event.kind !== "string") {
            throw new Error("invalid event frame");
          }
          queue.push(event);
          notify();
        })
        .catch((error: unknown) => {
          failure = new CoopError("invalid WebSocket event", { code: "invalid_response", cause: error });
          done = true;
          notify();
        });
    };
    const onClose = () => {
      done = true;
      notify();
    };
    const onError = (error: unknown) => {
      failure = new CoopError("WebSocket stream failed", { code: "stream_error", retryable: true, cause: error });
      done = true;
      notify();
    };
    const onAbort = () => {
      failure = abortError();
      done = true;
      socket.close(1000, "aborted");
      notify();
    };
    const attach = (type: "message" | "close" | "error", handler: (event: unknown) => void) => {
      if (socket.addEventListener) socket.addEventListener(type, handler);
      else socket[`on${type}`] = handler;
    };
    const detach = (type: "message" | "close" | "error", handler: (event: unknown) => void) => {
      if (socket.removeEventListener) socket.removeEventListener(type, handler);
      else socket[`on${type}`] = null;
    };
    attach("message", onMessage);
    attach("close", onClose);
    attach("error", onError);
    signal?.addEventListener("abort", onAbort, { once: true });

    try {
      while (true) {
        while (queue.length) {
          const event = queue.shift()!;
          if (event.seq <= cursor) continue;
          cursor = event.seq;
          yield event;
          if (event.kind === "finished" || isTerminal(event.data.status)) return;
        }
        if (done) {
          if (failure) throw failure;
          return;
        }
        await new Promise<void>((resolve) => {
          wake = resolve;
        });
      }
    } finally {
      signal?.removeEventListener("abort", onAbort);
      detach("message", onMessage);
      detach("close", onClose);
      detach("error", onError);
      try {
        socket.close(1000, "complete");
      } catch {
        // Already closed by the peer.
      }
    }
  }

  async *streamEvents(jobId: string, options: StreamOptions = {}): AsyncGenerator<CoopEvent> {
    let cursor = options.after ?? 0;
    if (!Number.isInteger(cursor) || cursor < -1) throw new RangeError("after must be -1 or a greater integer");
    const pollIntervalMs = options.pollIntervalMs ?? 1_000;
    assertPositive("pollIntervalMs", pollIntervalMs);

    if (options.preferWebSocket !== false && this.webSocketFactory) {
      try {
        const url = await this.streamUrl(jobId, cursor, options);
        for await (const event of this.socketEvents(url, cursor, options.signal)) {
          cursor = event.seq;
          yield event;
          if (event.kind === "finished" || isTerminal(event.data.status)) return;
        }
      } catch (error) {
        if (options.signal?.aborted) throw error;
        // Continue from the last sequence with dependency-free cursor replay.
      }
    }

    let checks = 0;
    let terminalProjectionSeen = false;
    while (true) {
      const pageLimit = 500;
      const page = await this.eventPage(jobId, {
        after: cursor,
        limit: pageLimit,
        signal: options.signal,
      });
      let terminalSeen = false;
      for (const event of page.events) {
        if (event.seq <= cursor) continue;
        cursor = event.seq;
        yield event;
        if (event.kind === "finished" || isTerminal(event.data.status)) terminalSeen = true;
      }
      // v0.2 finalization makes the terminal row last. Preserve any tail on a
      // legacy replay page before stopping rather than silently dropping it.
      if (terminalSeen) return;
      // A full page can have more durable history behind it. Drain backlog
      // without sleeping or consulting an already-terminal projection, which
      // would otherwise cut output off before the finished event's page.
      if (page.events.length >= pageLimit) continue;
      // Finalization commits projection and terminal event atomically, but it
      // can race between replay and GET. Require a replay after observing the
      // terminal projection before returning.
      if (terminalProjectionSeen) return;
      checks += 1;
      if (checks % 5 === 0) {
        const view = await this.get(jobId, { signal: options.signal });
        if (isTerminal(view.status)) {
          terminalProjectionSeen = true;
          continue;
        }
      }
      await delay(pollIntervalMs, options.signal);
    }
  }

  /** Callback compatibility wrapper. Prefer `for await ... of streamEvents()`. */
  stream(
    jobId: string,
    onEvent: (event: CoopEvent) => void,
    options?: StreamOptions,
  ): () => void;
  /**
   * @deprecated A bare key is intentionally rejected. Use
   * `{ allowLegacyQueryKey: true, legacyApiKey: key }` for a trusted v0.1
   * server, or omit both options for ticket-based v0.2 streaming.
   */
  stream(
    jobId: string,
    onEvent: (event: CoopEvent) => void,
    legacyApiKey: string,
  ): () => void;
  stream(
    jobId: string,
    onEvent: (event: CoopEvent) => void,
    optionsOrLegacyKey: StreamOptions | string = {},
  ): () => void {
    if (typeof optionsOrLegacyKey === "string") {
      throw new CoopError(
        "a bare legacy API key is no longer accepted; explicitly set allowLegacyQueryKey",
        { code: "legacy_query_key_opt_in_required" },
      );
    }
    const options = optionsOrLegacyKey;
    const controller = new AbortController();
    const onExternalAbort = () => controller.abort(options.signal?.reason);
    if (options.signal?.aborted) controller.abort(options.signal.reason);
    else options.signal?.addEventListener("abort", onExternalAbort, { once: true });
    void (async () => {
      try {
        for await (const event of this.streamEvents(jobId, { ...options, signal: controller.signal })) {
          onEvent(event);
        }
      } catch (error) {
        if (!controller.signal.aborted) options.onError?.(error);
      } finally {
        options.signal?.removeEventListener("abort", onExternalAbort);
      }
    })();
    return () => controller.abort();
  }
}
