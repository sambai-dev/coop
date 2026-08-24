export interface Limits {
  wall_seconds?: number;
  cpu_seconds?: number;
  mem_mb?: number;
  max_pids?: number;
  max_file_mb?: number;
  allow_network?: boolean;
}

export interface SubmitResponse {
  job_id: string;
  status: string;
  stream_url: string;
  replay_url: string;
}

export interface JobView {
  job_id: string;
  tenant: string;
  language: string;
  status: string;
  created_at_ms: number;
  started_at_ms?: number;
  finished_at_ms?: number;
  exit_code?: number;
}

export interface CoopEvent {
  seq: number;
  ts_ms: number;
  kind: "started" | "stdout" | "stderr" | "violation" | "truncated" | "finished";
  data: Record<string, unknown>;
}

export interface JobResult {
  job_id?: string;
  status: string;
  exit_code?: number;
  duration_ms?: number;
  stdout: string;
  stderr: string;
  truncated?: boolean;
  violations?: Record<string, unknown>[];
}

const TERMINAL = new Set(["succeeded", "failed", "timed_out", "oom_killed", "cancelled", "error"]);

const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

export class Coop {
  constructor(
    public baseUrl: string,
    public apiKey: string,
  ) {
    this.baseUrl = baseUrl.replace(/\/+$/, "");
  }

  private async request<T>(method: string, path: string, body?: unknown): Promise<T> {
    const res = await fetch(this.baseUrl + path, {
      method,
      headers: {
        Authorization: `Bearer ${this.apiKey}`,
        ...(body !== undefined ? { "Content-Type": "application/json" } : {}),
      },
      body: body !== undefined ? JSON.stringify(body) : undefined,
    });
    const text = await res.text();
    if (!res.ok) throw new Error(`coop ${res.status}: ${text}`);
    return (text ? JSON.parse(text) : null) as T;
  }

  submit(language: string, code: string, opts?: { stdin?: string; limits?: Limits }): Promise<SubmitResponse> {
    return this.request("POST", "/v1/jobs", {
      language,
      code,
      ...(opts?.stdin !== undefined ? { stdin: opts.stdin } : {}),
      ...(opts?.limits ? { limits: opts.limits } : {}),
    });
  }

  get(jobId: string): Promise<JobView> {
    return this.request("GET", `/v1/jobs/${jobId}`);
  }

  jobs(limit = 50): Promise<JobView[]> {
    return this.request("GET", `/v1/jobs?limit=${limit}`);
  }

  replay(jobId: string): Promise<CoopEvent[]> {
    return this.request("GET", `/v1/jobs/${jobId}/replay`);
  }

  async wait(jobId: string, timeoutMs = 60_000, pollMs = 250): Promise<JobView> {
    const deadline = Date.now() + timeoutMs;
    while (Date.now() < deadline) {
      const view = await this.get(jobId);
      if (TERMINAL.has(view.status)) return view;
      await sleep(pollMs);
    }
    throw new Error(`job ${jobId} still running after ${timeoutMs}ms`);
  }

  async result(jobId: string, timeoutMs = 60_000): Promise<JobResult> {
    // One-call fast path: newer servers fold the event log into a flat
    // result server-side and wait for us (202 = still running, partial).
    try {
      return await this.request(
        "GET",
        `/v1/jobs/${jobId}/result?wait_seconds=${Math.ceil(timeoutMs / 1000)}`,
      );
    } catch (err) {
      const msg = String((err as Error)?.message ?? "");
      if (!/: (404|405):/.test(msg)) throw err;
      // Older server without /result: fall back to poll + replay.
    }
    const view = await this.wait(jobId, timeoutMs);
    const events = await this.replay(jobId);
    const stdout: string[] = [];
    const stderr: string[] = [];
    for (const e of events) {
      const line = String((e.data as { line?: string }).line ?? "");
      if (e.kind === "stdout") stdout.push(line);
      else if (e.kind === "stderr") stderr.push(line);
    }
    return { status: view.status, exit_code: view.exit_code, stdout: stdout.join("\n"), stderr: stderr.join("\n") };
  }

  stream(jobId: string, onEvent: (e: CoopEvent) => void, keyForWs?: string): () => void {
    const base = this.baseUrl.replace(/^http/, "ws");
    const key = encodeURIComponent(keyForWs ?? this.apiKey);
    const ws = new WebSocket(`${base}/v1/jobs/${jobId}/stream?key=${key}`);
    ws.onmessage = (m) => {
      try {
        onEvent(JSON.parse(String(m.data)) as CoopEvent);
      } catch {}
    };
    return () => ws.close();
  }
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const base = process.argv[2] ?? "http://127.0.0.1:7300";
  const key = process.env.COOP_API_KEY ?? "coop-dev-key";
  const coop = new Coop(base, key);
  const job = await coop.submit("node", "console.log('hello from the coop sdk')");
  console.log("submitted:", job.job_id);
  console.log(await coop.result(job.job_id));
}
