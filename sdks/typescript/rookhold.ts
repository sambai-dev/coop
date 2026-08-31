/** Typed browser and Node.js client for the Rookhold execution API. */

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
export const ISOLATION_CLASSES = [
  "none",
  "linux-shared-kernel",
  "gvisor-application-kernel",
  "wasm-capability",
  "hardware-vm",
  "confidential-vm",
] as const;
export type IsolationClass = (typeof ISOLATION_CLASSES)[number];
/** @deprecated Use `IsolationClass`. */
export type IsolationLevel = IsolationClass;
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

export interface JobRequirements {
  minimum_isolation: IsolationClass;
}

/** Whether an observed provider class satisfies an atomic minimum requirement. */
export function isolationSatisfies(
  actual: IsolationClass,
  minimum: IsolationClass,
): boolean {
  if (minimum === "none") return true;
  if (minimum === "wasm-capability") return actual === "wasm-capability";
  if (actual === "wasm-capability" || actual === "none") return false;
  const processClasses: readonly IsolationClass[] = [
    "linux-shared-kernel",
    "gvisor-application-kernel",
    "hardware-vm",
    "confidential-vm",
  ];
  const actualRank = processClasses.indexOf(actual);
  const minimumRank = processClasses.indexOf(minimum);
  return minimumRank >= 0 && actualRank >= minimumRank;
}

export interface JobSpec {
  language: Language | (string & {});
  code: string;
  stdin?: string;
  limits?: Limits;
  requirements?: JobRequirements;
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
  /** Absent on pre-v0.4 servers; v0.4 defaults this to `none`. */
  requirements?: JobRequirements;
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
  /** Absent on pre-v0.4 execution evidence. */
  requirements?: JobRequirements;
  /** Null until the provider crosses its observed workload-ready boundary. */
  isolation_class?: IsolationClass | null;
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

/** Submission body plus typed response-header evidence. */
export interface SubmitResult {
  job: SubmitResponse;
  /** Relative or absolute job URL from the HTTP Location header. */
  location?: string;
  /** True when the server replayed a prior identical Idempotency-Key request. */
  idempotency_replayed: boolean;
}

export interface CancellationResponse {
  job: JobView;
  cancellation_requested: boolean;
  already_terminal: boolean;
}

/** Normalized evidence for the empty 200 returned by v0.1-v0.3 servers. */
export interface LegacyCancellationResponse {
  job: null;
  cancellation_requested: true;
  already_terminal: false;
}

export type CancellationResult = CancellationResponse | LegacyCancellationResponse;

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
  /** Absent on pre-v0.4 projections. */
  isolation_class?: IsolationClass | null;
  bootstrap_ready: boolean | null;
  isolated: boolean | null;
  seccomp: boolean | null;
  network_allowed: boolean | null;
  networking: "disabled" | "host" | null;
  private_rootfs: boolean | null;
  dedicated_bootstrap: boolean | null;
  limit_enforcement: LimitEnforcement | null;
  /** Provider provenance is absent on pre-v0.4 or not-ready projections. */
  runtime_version?: string | null;
  runtime_sha256?: string | null;
  rootfs_sha256?: string | null;
  config_sha256?: string | null;
}

export interface JobDetail extends JobView {
  requested_spec: StoredJobSpec;
  effective_spec: EffectiveJobSpec | null;
  execution_policy: ExecutionPolicy;
  receipt: Receipt | null;
  receipt_sha256: string | null;
  attestation: JobAttestationStatus;
}

export interface JobAttestationStatus {
  available: boolean;
  /** Authoritative tenant carried by both portable evidence files. */
  tenant: string | null;
  key_id: string | null;
  receipt_sha256: string | null;
  result_media_type: string | null;
  result_sha256: string | null;
  result_size_bytes: number | null;
  envelope_sha256: string | null;
  envelope_size_bytes: number | null;
  envelope_url: string | null;
  result_artifact_url: string | null;
}

export type Job = JobView | JobDetail;

export interface RookholdEvent {
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

/** An event after the caller has validated its evidence fields. */
export interface HashedRookholdEvent extends RookholdEvent {
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
  requirements?: JobRequirements;
  minimum_isolation?: IsolationClass;
  isolation_class?: IsolationClass;
  effective_limits?: EffectiveLimits;
  limit_enforcement?: LimitEnforcement;
  code_sha256?: string;
  stdin_sha256?: string;
  policy_sha256?: string;
  runtime_version?: string | null;
  runtime_sha256?: string | null;
  rootfs_sha256?: string | null;
  config_sha256?: string | null;
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
  events: RookholdEvent[];
  next_cursor: number | null;
}

export interface StreamTicket {
  ticket: string;
  stream_url: string;
  expires_at_ms: number;
}

export interface ExecutionCapabilities {
  backend: string;
  isolation_class: IsolationClass;
  isolated: boolean;
  private_rootfs: boolean;
  dedicated_bootstrap: boolean;
  seccomp: boolean;
  networking: "disabled" | "host";
  limit_enforcement: LimitEnforcement;
}

export interface LimitCapabilities {
  wall_seconds_max: number;
  cpu_seconds_max: number;
  mem_mb_max: number;
  concurrent_mem_mb_max: number;
  pids_max: number;
  file_mb_max: number;
  output_lines_max: number;
  output_bytes_per_stream_max: number;
  output_record_bytes_max: number;
  code_bytes_max: number;
  stdin_bytes_max: number;
}

export interface FeatureCapabilities {
  result_wait: boolean;
  cancellation: boolean;
  event_cursors: boolean;
  stream_tickets: boolean;
  receipts: boolean;
  signed_attestations: boolean;
}

export interface AttestationCapabilities {
  enabled: boolean;
  algorithm: string | null;
  envelope_format: string | null;
  key_id: string | null;
  public_key_url: string | null;
}

export interface Capabilities {
  version: string;
  languages: string[];
  execution: ExecutionCapabilities;
  limits: LimitCapabilities;
  features: FeatureCapabilities;
  attestations: AttestationCapabilities;
}

export interface AttestationPublicKey {
  algorithm: string;
  key_id: string;
  public_key_pem: string;
  trust_notice: string;
}

/**
 * Exact response bytes plus HTTP integrity metadata. `sha256` has been checked
 * against X-Content-Sha256; no DSSE signature or key trust has been verified.
 */
export interface ArtifactDownload {
  content: Uint8Array;
  contentType: string;
  contentLength: number;
  sha256: string;
}

export interface WhoAmI {
  tenant: string;
  principal_id: string;
  credential_id: string | null;
  auth_method: string;
  scopes: string[];
  expires_at_ms: number | null;
}

export interface RequestOptions {
  signal?: AbortSignal | undefined;
  timeoutMs?: number | undefined;
}

export interface SubmitOptions extends RequestOptions {
  stdin?: string | undefined;
  limits?: Limits | undefined;
  requirements?: JobRequirements | undefined;
  /**
   * Stable key for safely reconciling an ambiguously acknowledged submission.
   * The value must contain 1-128 visible ASCII bytes.
   */
  idempotencyKey?: string | undefined;
  /**
   * Retry one transport-level ambiguous failure. The same Idempotency-Key is
   * reused for both attempts; one is generated when `idempotencyKey` is absent.
   * Enable this only when the target server enforces submission idempotency.
   */
  retryAmbiguous?: boolean | undefined;
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

/** Fetch contract shared by browsers and Node.js without the DOM-only RequestInfo alias. */
export type FetchLike = (
  input: string | URL | Request,
  init?: RequestInit,
) => Promise<Response>;

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

export interface RookholdErrorInit {
  status?: number | undefined;
  code?: string | undefined;
  requestId?: string | undefined;
  retryable?: boolean | undefined;
  body?: string | undefined;
  retryAfterMs?: number | undefined;
  /** Submission key callers can persist after an ambiguous acknowledgement. */
  idempotencyKey?: string | undefined;
  cause?: unknown;
}

export class RookholdError extends Error {
  readonly status: number | undefined;
  readonly code: string;
  readonly requestId: string | undefined;
  readonly retryable: boolean;
  readonly body: string;
  readonly retryAfterMs: number | undefined;
  readonly idempotencyKey: string | undefined;
  override readonly cause: unknown;

  constructor(message: string, init: RookholdErrorInit = {}) {
    super(message);
    this.name = "RookholdError";
    this.status = init.status;
    this.code = init.code ?? "unknown_error";
    this.requestId = init.requestId;
    this.retryable = init.retryable ?? false;
    this.body = init.body ?? "";
    this.retryAfterMs = init.retryAfterMs;
    this.idempotencyKey = init.idempotencyKey;
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

function parseHttpError(status: number, body: string, headers: Headers): RookholdError {
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
  return new RookholdError(message, {
    status,
    code,
    requestId,
    retryable,
    body,
    retryAfterMs: retryAfterMs(headers),
  });
}

function abortError(message = "operation aborted"): RookholdError {
  return new RookholdError(message, { code: "request_aborted", retryable: false });
}

function throwIfAborted(signal?: AbortSignal): void {
  if (signal?.aborted) throw abortError();
}

function validateIdempotencyKey(value: string): string {
  if (value.length < 1 || value.length > 128 || !/^[\x21-\x7e]+$/.test(value)) {
    throw new TypeError("idempotencyKey must contain 1-128 visible ASCII bytes");
  }
  return value;
}

let fallbackIdempotencyCounter = 0;

function generatedIdempotencyKey(): string {
  const crypto = globalThis.crypto;
  if (typeof crypto?.randomUUID === "function") return crypto.randomUUID();
  if (typeof crypto?.getRandomValues === "function") {
    const bytes = crypto.getRandomValues(new Uint8Array(16));
    bytes[6] = (bytes[6]! & 0x0f) | 0x40;
    bytes[8] = (bytes[8]! & 0x3f) | 0x80;
    const hex = Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0"));
    return `${hex.slice(0, 4).join("")}-${hex.slice(4, 6).join("")}-${hex.slice(6, 8).join("")}-${hex.slice(8, 10).join("")}-${hex.slice(10).join("")}`;
  }
  // Node 18 does not expose Web Crypto globally in every launch mode. An
  // idempotency key is a collision-avoidance token, not an authorization
  // secret, so combine time, a process-local counter, and 128 pseudo-random
  // bits as a portable last resort.
  fallbackIdempotencyCounter = (fallbackIdempotencyCounter + 1) >>> 0;
  const random = Array.from({ length: 4 }, () =>
    Math.floor(Math.random() * 0x1_0000_0000)
      .toString(16)
      .padStart(8, "0"),
  ).join("");
  return `coop-${Date.now().toString(36)}-${fallbackIdempotencyCounter.toString(36)}-${random}`;
}

function isAmbiguousTransportFailure(error: unknown): error is RookholdError {
  return (
    error instanceof RookholdError &&
    (error.code === "request_timeout" || error.code === "transport_error")
  );
}

function withIdempotencyKey(error: RookholdError, idempotencyKey: string): RookholdError {
  if (error.idempotencyKey === idempotencyKey) return error;
  const contextual = new RookholdError(error.message, {
    status: error.status,
    code: error.code,
    requestId: error.requestId,
    retryable: error.retryable,
    body: error.body,
    retryAfterMs: error.retryAfterMs,
    idempotencyKey,
    cause: error.cause,
  });
  if (error.stack !== undefined) contextual.stack = error.stack;
  return contextual;
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

async function sha256Hex(bytes: Uint8Array): Promise<string> {
  const subtle = globalThis.crypto?.subtle;
  if (!subtle) {
    throw new RookholdError("this runtime does not provide Web Crypto SHA-256", {
      code: "digest_unavailable",
      retryable: false,
    });
  }
  // Copy into a plain ArrayBuffer-backed view so this remains accepted by
  // strict DOM typings even when callers hold a shared or offset view.
  const input = Uint8Array.from(bytes);
  const digest = new Uint8Array(await subtle.digest("SHA-256", input));
  return Array.from(digest, (value) => value.toString(16).padStart(2, "0")).join("");
}

interface RequestResult<T> {
  data: T;
  status: number;
  headers: Headers;
}

interface RequestPolicy {
  headers?: Readonly<Record<string, string>>;
  ambiguousFailureRetryable?: boolean;
  idempotencyKey?: string;
}

export class Rookhold {
  readonly baseUrl: string;
  readonly timeoutMs: number;
  readonly #apiKey: string;
  private readonly fetcher: FetchLike;
  private readonly webSocketFactory: WebSocketFactory | undefined;

  /** @deprecated Prefer keeping the credential outside application state. */
  get apiKey(): string {
    return this.#apiKey;
  }

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
    this.#apiKey = apiKey;

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
    policy: RequestPolicy = {},
  ): Promise<RequestResult<T>> {
    const timeoutMs = options.timeoutMs ?? this.timeoutMs;
    assertPositive("timeoutMs", timeoutMs);
    throwIfAborted(options.signal);
    const ambiguousFailureRetryable =
      policy.ambiguousFailureRetryable ?? method.toUpperCase() !== "POST";
    let serializedBody: string | undefined;
    if (body !== undefined) {
      try {
        serializedBody = JSON.stringify(body);
      } catch (cause) {
        throw new RookholdError("request body is not JSON serializable", {
          code: "invalid_request",
          retryable: false,
          cause,
        });
      }
    }

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
            Authorization: `Bearer ${this.#apiKey}`,
            ...(body !== undefined ? { "Content-Type": "application/json" } : {}),
            "X-Rookhold-Client": "typescript/0.6.0",
            "X-Coop-Client": "typescript/0.6.0",
            ...policy.headers,
          },
          ...(serializedBody !== undefined ? { body: serializedBody } : {}),
        });
      } catch (cause) {
        if (timedOut) {
          throw new RookholdError(`request timed out after ${timeoutMs}ms`, {
            code: "request_timeout",
            retryable: ambiguousFailureRetryable,
            idempotencyKey: policy.idempotencyKey,
            cause,
          });
        }
        if (options.signal?.aborted) throw abortError();
        throw new RookholdError(cause instanceof Error ? cause.message : String(cause), {
          code: "transport_error",
          retryable: ambiguousFailureRetryable,
          idempotencyKey: policy.idempotencyKey,
          cause,
        });
      }

      let text: string;
      try {
        text = await response.text();
      } catch (cause) {
        if (timedOut) {
          throw new RookholdError(`request timed out after ${timeoutMs}ms`, {
            code: "request_timeout",
            retryable: ambiguousFailureRetryable,
            idempotencyKey: policy.idempotencyKey,
            cause,
          });
        }
        if (options.signal?.aborted) throw abortError();
        throw new RookholdError("failed to read the server response", {
          code: "transport_error",
          retryable: ambiguousFailureRetryable,
          idempotencyKey: policy.idempotencyKey,
          cause,
        });
      }
      if (!response.ok) throw parseHttpError(response.status, text, response.headers);
      if (!text) {
        return { data: undefined as T, status: response.status, headers: response.headers };
      }
      try {
        return {
          data: JSON.parse(text) as T,
          status: response.status,
          headers: response.headers,
        };
      } catch (cause) {
        throw new RookholdError("server returned invalid JSON", {
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
    policy: RequestPolicy = {},
  ): Promise<T> {
    return (await this.requestResult<T>(method, path, body, options, query, policy)).data;
  }

  private async downloadArtifact(
    path: string,
    accept: string,
    options: RequestOptions = {},
  ): Promise<ArtifactDownload> {
    const timeoutMs = options.timeoutMs ?? this.timeoutMs;
    assertPositive("timeoutMs", timeoutMs);
    throwIfAborted(options.signal);
    const requestedUrl = this.url(path);
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
        response = await this.fetcher(requestedUrl, {
          method: "GET",
          redirect: "error",
          signal: controller.signal,
          headers: {
            Accept: accept,
            Authorization: `Bearer ${this.#apiKey}`,
            "X-Rookhold-Client": "typescript/0.6.0",
            "X-Coop-Client": "typescript/0.6.0",
          },
        });
      } catch (cause) {
        if (timedOut) {
          throw new RookholdError(`request timed out after ${timeoutMs}ms`, {
            code: "request_timeout",
            retryable: true,
            cause,
          });
        }
        if (options.signal?.aborted) throw abortError();
        throw new RookholdError(cause instanceof Error ? cause.message : String(cause), {
          code: "transport_error",
          retryable: true,
          cause,
        });
      }

      if (response.redirected) {
        throw new RookholdError("refused a redirected artifact response", {
          status: response.status,
          code: "unsafe_redirect",
        });
      }
      if (response.url) {
        let responseUrl: URL;
        try {
          responseUrl = new URL(response.url);
        } catch (cause) {
          throw new RookholdError("artifact response URL did not have a valid HTTP origin", {
            status: response.status,
            code: "unsafe_redirect",
            cause,
          });
        }
        if (responseUrl.origin !== requestedUrl.origin) {
          throw new RookholdError("refused a cross-origin artifact response", {
            status: response.status,
            code: "unsafe_redirect",
          });
        }
      }

      if (!response.ok) {
        let text: string;
        try {
          text = await response.text();
        } catch (cause) {
          if (timedOut) {
            throw new RookholdError(`request timed out after ${timeoutMs}ms`, {
              code: "request_timeout",
              retryable: true,
              cause,
            });
          }
          if (options.signal?.aborted) throw abortError();
          throw new RookholdError("failed to read the server response", {
            code: "transport_error",
            retryable: true,
            cause,
          });
        }
        throw parseHttpError(response.status, text, response.headers);
      }

      let content: Uint8Array;
      try {
        content = new Uint8Array(await response.arrayBuffer());
      } catch (cause) {
        if (timedOut) {
          throw new RookholdError(`request timed out after ${timeoutMs}ms`, {
            code: "request_timeout",
            retryable: true,
            cause,
          });
        }
        if (options.signal?.aborted) throw abortError();
        throw new RookholdError("failed to read the artifact response", {
          code: "transport_error",
          retryable: true,
          cause,
        });
      }

      const digestHeader = response.headers.get("x-content-sha256");
      if (digestHeader === null) {
        throw new RookholdError("artifact response omitted X-Content-Sha256", {
          status: response.status,
          code: "invalid_response",
        });
      }
      if (!/^[0-9a-fA-F]{64}$/.test(digestHeader)) {
        throw new RookholdError("artifact response contained a malformed X-Content-Sha256", {
          status: response.status,
          code: "invalid_response",
        });
      }
      const actualSha256 = await sha256Hex(content);
      if (actualSha256 !== digestHeader.toLowerCase()) {
        throw new RookholdError("artifact bytes did not match X-Content-Sha256", {
          status: response.status,
          code: "content_digest_mismatch",
          retryable: true,
        });
      }

      const contentType = response.headers.get("content-type");
      if (contentType === null || contentType.trim() === "") {
        throw new RookholdError("artifact response omitted Content-Type", {
          status: response.status,
          code: "invalid_response",
        });
      }
      const declaredLength = response.headers.get("content-length");
      if (declaredLength !== null) {
        if (!/^\d+$/.test(declaredLength)) {
          throw new RookholdError("artifact response contained a malformed Content-Length", {
            status: response.status,
            code: "invalid_response",
          });
        }
        const parsedLength = Number(declaredLength);
        if (!Number.isSafeInteger(parsedLength)) {
          throw new RookholdError("artifact Content-Length exceeded the safe integer range", {
            status: response.status,
            code: "invalid_response",
          });
        }
        if (parsedLength !== content.byteLength) {
          throw new RookholdError("artifact response body did not match Content-Length", {
            status: response.status,
            code: "transport_error",
            retryable: true,
          });
        }
      }
      return {
        content,
        contentType,
        contentLength: content.byteLength,
        sha256: actualSha256,
      };
    } finally {
      clearTimeout(timer);
      options.signal?.removeEventListener("abort", onAbort);
    }
  }

  async submit(
    language: Language | (string & {}),
    code: string,
    options: SubmitOptions = {},
  ): Promise<SubmitResponse> {
    return (await this.submitResult(language, code, options)).job;
  }

  async submitResult(
    language: Language | (string & {}),
    code: string,
    options: SubmitOptions = {},
  ): Promise<SubmitResult> {
    const {
      signal,
      timeoutMs,
      stdin,
      limits,
      requirements,
      retryAmbiguous = false,
    } = options;
    const spec: JobSpec = { language, code };
    if (stdin !== undefined) spec.stdin = stdin;
    if (limits !== undefined) spec.limits = limits;
    if (requirements !== undefined) spec.requirements = requirements;
    const idempotencyKey =
      options.idempotencyKey === undefined
        ? retryAmbiguous
          ? generatedIdempotencyKey()
          : undefined
        : validateIdempotencyKey(options.idempotencyKey);
    const policy: RequestPolicy = {
      ambiguousFailureRetryable: idempotencyKey !== undefined,
      ...(idempotencyKey === undefined
        ? {}
        : {
            idempotencyKey,
            headers: { "Idempotency-Key": idempotencyKey },
          }),
    };
    const attempts = retryAmbiguous ? 2 : 1;
    for (let attempt = 0; attempt < attempts; attempt += 1) {
      try {
        const response = await this.requestResult<SubmitResponse>(
          "POST",
          "/v1/jobs",
          spec,
          { signal, timeoutMs },
          undefined,
          policy,
        );
        const replayedHeader = response.headers.get("idempotency-replayed");
        if (
          replayedHeader !== null &&
          replayedHeader !== "true" &&
          replayedHeader !== "false"
        ) {
          throw new RookholdError("invalid Idempotency-Replayed response header", {
            status: response.status,
            code: "invalid_response",
          });
        }
        const location = response.headers.get("location");
        return {
          job: response.data,
          idempotency_replayed: replayedHeader === "true",
          ...(location === null ? {} : { location }),
        };
      } catch (error) {
        if (attempt + 1 < attempts && isAmbiguousTransportFailure(error)) {
          if (signal?.aborted) {
            const aborted = abortError();
            throw idempotencyKey === undefined
              ? aborted
              : withIdempotencyKey(aborted, idempotencyKey);
          }
          continue;
        }
        throw idempotencyKey !== undefined && error instanceof RookholdError
          ? withIdempotencyKey(error, idempotencyKey)
          : error;
      }
    }
    throw new Error("unreachable submission retry state");
  }

  get(jobId: string, options: RequestOptions = {}): Promise<JobDetail> {
    return this.request("GET", this.jobPath(jobId), undefined, options);
  }

  async cancelResult(
    jobId: string,
    options: RequestOptions = {},
  ): Promise<CancellationResult> {
    const result = await this.requestResult<CancellationResponse | undefined>(
      "DELETE",
      this.jobPath(jobId),
      undefined,
      options,
    );
    if (result.data === undefined) {
      // v0.1-v0.3 returned an empty 200, which proves acceptance but carries no
      // projection. Avoid an extra request while preserving that evidence.
      return {
        job: null,
        cancellation_requested: true,
        already_terminal: false,
      };
    }
    if (
      !result.data.job ||
      typeof result.data.job !== "object" ||
      typeof result.data.cancellation_requested !== "boolean" ||
      typeof result.data.already_terminal !== "boolean"
    ) {
      throw new RookholdError("invalid cancellation response", { code: "invalid_response" });
    }
    return result.data;
  }

  async cancel(jobId: string, options: RequestOptions = {}): Promise<void> {
    await this.cancelResult(jobId, options);
  }

  whoami(options: RequestOptions = {}): Promise<WhoAmI> {
    return this.request("GET", "/v1/whoami", undefined, options);
  }

  capabilities(options: RequestOptions = {}): Promise<Capabilities> {
    return this.request("GET", "/v1/capabilities", undefined, options);
  }

  /** Discovery only: pin this key out of band before treating it as trusted. */
  attestationPublicKey(options: RequestOptions = {}): Promise<AttestationPublicKey> {
    return this.request("GET", "/v1/attestation/public-key", undefined, options);
  }

  /** Download exact persisted DSSE bytes and validate their HTTP digest. */
  downloadAttestation(
    jobId: string,
    options: RequestOptions = {},
  ): Promise<ArtifactDownload> {
    return this.downloadArtifact(
      `${this.jobPath(jobId)}/attestation`,
      "application/vnd.dsse.envelope.v1+json",
      options,
    );
  }

  /** Download exact signed-subject bytes and validate their HTTP digest. */
  downloadResultArtifact(
    jobId: string,
    options: RequestOptions = {},
  ): Promise<ArtifactDownload> {
    return this.downloadArtifact(
      `${this.jobPath(jobId)}/result-artifact`,
      "application/vnd.coop.execution-result.v1+json",
      options,
    );
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
      throw new RookholdError("invalid job list envelope", { code: "invalid_response" });
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
    const raw = await this.request<EventPage | RookholdEvent[]>(
      "GET",
      `${this.jobPath(jobId)}/replay`,
      undefined,
      options,
      { after: after === undefined ? undefined : Math.max(0, after), limit },
    );
    if (Array.isArray(raw)) {
      if (raw.some((event) => !Number.isSafeInteger(event.seq))) {
        throw new RookholdError("invalid event sequence", { code: "invalid_response" });
      }
      return {
        events: after === undefined ? raw : raw.filter((event) => event.seq > after),
        next_cursor: null,
      };
    }
    if (!raw || !Array.isArray(raw.events)) {
      throw new RookholdError("invalid event replay envelope", { code: "invalid_response" });
    }
    if (
      raw.next_cursor !== null &&
      !Number.isSafeInteger(raw.next_cursor)
    ) {
      throw new RookholdError("invalid event replay cursor", { code: "invalid_response" });
    }
    if (raw.events.some((event) => !Number.isSafeInteger(event.seq))) {
      throw new RookholdError("invalid event sequence", { code: "invalid_response" });
    }
    return raw;
  }

  async replay(
    jobId: string,
    after?: number,
    limit = 1_000,
    options: RequestOptions = {},
  ): Promise<RookholdEvent[]> {
    const events: RookholdEvent[] = [];
    let cursor = after;
    while (true) {
      const page = await this.eventPage(jobId, { ...options, after: cursor, limit });
      events.push(...page.events);
      if (page.next_cursor === null) return events;
      if (
        !Number.isSafeInteger(page.next_cursor) ||
        page.next_cursor <= Math.max(0, cursor ?? 0)
      ) {
        throw new RookholdError("event cursor did not advance", { code: "invalid_response" });
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
        throw new RookholdError(`job ${jobId} still running after ${timeoutLabelMs}ms`, {
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
        throw new RookholdError(`job ${jobId} still running after ${timeoutLabelMs}ms`, {
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
        throw new RookholdError(`job ${jobId} still running after ${timeoutMs}ms`, {
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
          !(error instanceof RookholdError) ||
          (error.code !== "http_404" && error.code !== "http_405")
        ) {
          throw error;
        }
        return this.resultViaPolling(jobId, deadline, signal);
      }
      if (Date.now() >= deadline) {
        throw new RookholdError(`job ${jobId} still running after ${timeoutMs}ms`, { code: "job_wait_timeout", retryable: true });
      }
      await delay(Math.min(1_000, Math.max(0, deadline - Date.now())), signal);
    }
  }

  private async resultViaPolling(jobId: string, deadline: number, signal?: AbortSignal): Promise<JobResult> {
    const waitBudget = deadline - Date.now();
    if (waitBudget <= 0) {
      throw new RookholdError(`job ${jobId} result deadline expired`, {
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
    let terminalEventSeen = false;
    let terminalCatchupPages = 0;
    const maxTerminalCatchupPages = 3;
    while (true) {
      const remaining = deadline - Date.now();
      if (remaining <= 0) {
        throw new RookholdError(`job ${jobId} result deadline expired`, {
          code: "job_wait_timeout",
          retryable: true,
        });
      }
      const page = await this.eventPage(jobId, {
        after,
        signal,
        timeoutMs: remaining,
      });
      const pageStart = Math.max(0, after ?? 0);
      let pageMax = pageStart;
      for (const event of page.events) {
        if (event.seq <= pageStart) continue;
        if (event.seq <= pageMax) {
          throw new RookholdError("event sequence did not advance", {
            code: "invalid_response",
          });
        }
        pageMax = event.seq;
        after = pageMax;
        const line = typeof event.data.line === "string" ? event.data.line : "";
        if (event.kind === "stdout") stdout.push(line);
        else if (event.kind === "stderr") stderr.push(line);
        else if (event.kind === "truncated") truncated = true;
        else if (event.kind === "violation") violations.push(event.data);
        if (event.kind === "finished" || isTerminal(event.data.status)) {
          terminalEventSeen = true;
        }
      }
      if (page.next_cursor !== null) {
        if (
          !Number.isSafeInteger(page.next_cursor) ||
          page.next_cursor <= pageStart ||
          page.next_cursor < pageMax
        ) {
          throw new RookholdError("event cursor did not advance", { code: "invalid_response" });
        }
        after = page.next_cursor;
        continue;
      }
      if (terminalEventSeen) break;

      // The terminal projection can become visible just before a replay read
      // observes its final event. Retry a few short, cursor-resuming pages. Old
      // servers that never wrote `finished` still return their accumulated
      // output after this bounded compatibility allowance.
      terminalCatchupPages += 1;
      if (terminalCatchupPages >= maxTerminalCatchupPages) break;
      const sleepBudget = deadline - Date.now();
      if (sleepBudget <= 0) {
        throw new RookholdError(`job ${jobId} result deadline expired`, {
          code: "job_wait_timeout",
          retryable: true,
        });
      }
      await delay(Math.min(25, sleepBudget), signal);
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
      throw new RookholdError("invalid stream URL", { code: "invalid_response" });
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
        throw new RookholdError("invalid stream ticket response", { code: "invalid_response" });
      }
      const url = this.websocketUrl(ticket.stream_url);
      if (!url.searchParams.has("ticket")) url.searchParams.set("ticket", ticket.ticket);
      if (!url.searchParams.has("after")) url.searchParams.set("after", String(wireAfter));
      return url;
    } catch (error) {
      if (!(error instanceof RookholdError) || (error.status !== 404 && error.status !== 405)) throw error;
      // A structured Rookhold `job_not_found` is not evidence that stream tickets
      // are unsupported. Query-key compatibility is explicit and limited to
      // the unstructured HTTP code emitted by legacy servers.
      const legacyEndpointMissing = error.code === "http_404" || error.code === "http_405";
      if (options.allowLegacyQueryKey !== true || !legacyEndpointMissing) {
        throw new RookholdError("server does not support stream tickets and legacy key URLs are disabled", {
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

  private async *socketEvents(url: URL, after: number, signal?: AbortSignal): AsyncGenerator<RookholdEvent> {
    if (!this.webSocketFactory) return;
    if (signal?.aborted) throw abortError();
    const socket = this.webSocketFactory(url.toString());
    // Enqueue raw frames synchronously. Decoding inside the generator preserves
    // WebSocket arrival order even when Blob-like `text()` calls resolve out of
    // order.
    const queue: unknown[] = [];
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
      queue.push(eventData(message));
      notify();
    };
    const onClose = () => {
      done = true;
      notify();
    };
    const onError = (error: unknown) => {
      failure = new RookholdError("WebSocket stream failed", { code: "stream_error", retryable: true, cause: error });
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
    if (signal?.aborted) onAbort();

    try {
      while (true) {
        while (queue.length) {
          throwIfAborted(signal);
          let event: RookholdEvent;
          try {
            const text = await messageText(queue.shift());
            throwIfAborted(signal);
            event = JSON.parse(text) as RookholdEvent;
            if (!Number.isSafeInteger(event.seq) || typeof event.kind !== "string") {
              throw new Error("invalid event frame");
            }
          } catch (error) {
            if (signal?.aborted) throw abortError();
            throw new RookholdError("invalid WebSocket event", {
              code: "invalid_response",
              cause: error,
            });
          }
          if (event.seq <= cursor) continue;
          cursor = event.seq;
          yield event;
          throwIfAborted(signal);
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

  async *streamEvents(jobId: string, options: StreamOptions = {}): AsyncGenerator<RookholdEvent> {
    let cursor = options.after ?? 0;
    if (!Number.isInteger(cursor) || cursor < -1) throw new RangeError("after must be -1 or a greater integer");
    const pollIntervalMs = options.pollIntervalMs ?? 1_000;
    assertPositive("pollIntervalMs", pollIntervalMs);

    let terminalEventSeen = false;
    if (options.preferWebSocket !== false && this.webSocketFactory) {
      try {
        const url = await this.streamUrl(jobId, cursor, options);
        for await (const event of this.socketEvents(url, cursor, options.signal)) {
          throwIfAborted(options.signal);
          cursor = event.seq;
          yield event;
          throwIfAborted(options.signal);
          if (event.kind === "finished" || isTerminal(event.data.status)) {
            terminalEventSeen = true;
            break;
          }
        }
      } catch (error) {
        if (options.signal?.aborted) throw error;
        // Continue from the last sequence with dependency-free cursor replay.
      }
    }

    let checks = 0;
    // Even after a terminal WebSocket event, perform one durable replay. This
    // catches a legacy tail and fences the live/durable hand-off before return.
    let terminalProjectionSeen = terminalEventSeen;
    while (true) {
      throwIfAborted(options.signal);
      const pageLimit = 500;
      const pageStart = cursor;
      const page = await this.eventPage(jobId, {
        after: cursor,
        limit: pageLimit,
        signal: options.signal,
      });
      for (const event of page.events) {
        throwIfAborted(options.signal);
        if (event.seq <= cursor) continue;
        cursor = event.seq;
        yield event;
        throwIfAborted(options.signal);
        if (event.kind === "finished" || isTerminal(event.data.status)) {
          terminalEventSeen = true;
        }
      }
      // A full page can have more durable history behind it. Drain backlog
      // without sleeping or consulting an already-terminal projection, which
      // would otherwise cut output off before the finished event's page.
      if (page.events.length >= pageLimit) {
        if (cursor <= pageStart) {
          throw new RookholdError("event cursor did not advance", {
            code: "invalid_response",
          });
        }
        continue;
      }
      // Current finalization makes the terminal row last. A legacy server can
      // retain a tail after it, so only stop after draining a short final page.
      if (terminalEventSeen) return;
      // A terminal projection is only a hint to accelerate replay. Never let a
      // stale/empty page cut off the durable terminal event; keep polling until
      // that event is actually observed (or the caller aborts).
      if (terminalProjectionSeen) {
        await delay(pollIntervalMs, options.signal);
        continue;
      }
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
    onEvent: (event: RookholdEvent) => void,
    options?: StreamOptions,
  ): () => void;
  /**
   * @deprecated A bare key is intentionally rejected. Use
   * `{ allowLegacyQueryKey: true, legacyApiKey: key }` for a trusted v0.1
   * server, or omit both options for ticket-based streaming.
   */
  stream(
    jobId: string,
    onEvent: (event: RookholdEvent) => void,
    legacyApiKey: string,
  ): () => void;
  stream(
    jobId: string,
    onEvent: (event: RookholdEvent) => void,
    optionsOrLegacyKey: StreamOptions | string = {},
  ): () => void {
    if (typeof optionsOrLegacyKey === "string") {
      throw new RookholdError(
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
