"use client";

import { useState } from "react";

const starter = "print(sum(value * value for value in range(6)))";

export default function Page() {
  const [code, setCode] = useState(starter);
  const [language, setLanguage] = useState("python");
  const [result, setResult] = useState<Record<string, unknown> | null>(null);
  const [state, setState] = useState<"ready" | "running" | "failed">("ready");

  async function run() {
    setState("running");
    try {
      const response = await fetch("/api/run", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ language, code }),
      });
      const value = (await response.json()) as Record<string, unknown>;
      setResult(value);
      setState(response.ok ? "ready" : "failed");
    } catch {
      setResult({ error: "The runner did not return JSON. Check the server log." });
      setState("failed");
    }
  }

  return (
    <main>
      <header>
        <strong>Rookhold / starter</strong>
        <span>short jobs · hard limits · receipts</span>
      </header>
      <section className="workbench">
        <div className="compose">
          <div className="heading">
            <p>Code</p>
            <select value={language} onChange={(event) => setLanguage(event.target.value)}>
              <option>python</option>
              <option>node</option>
              <option>bash</option>
            </select>
          </div>
          <textarea
            aria-label="Code"
            spellCheck={false}
            value={code}
            onChange={(event) => setCode(event.target.value)}
          />
          <button onClick={run} disabled={state === "running"}>
            {state === "running" ? "Running…" : "Run with limits"}
          </button>
          <p className="warning">
            This starter requires a gVisor service. Add sign-in, user quotas, and abuse controls
            before production use.
          </p>
        </div>
        <div className="carbon" aria-live="polite">
          <p className="eyebrow">RUN OUTPUT</p>
          <pre>{String(result?.stdout ?? result?.error ?? "Output will appear here.")}</pre>
          <dl>
            <dt>Status</dt><dd>{String(result?.status ?? (state === "failed" ? "failed" : "not run"))}</dd>
            <dt>Isolation</dt><dd>{String(result?.isolation ?? "unknown")}</dd>
            <dt>Duration</dt><dd>{result?.durationMs ? `${result.durationMs} ms` : "—"}</dd>
          </dl>
        </div>
        <aside>
          <p className="eyebrow">RECEIPT RAIL</p>
          <p className="job">{String(result?.jobId ?? "No receipt yet")}</p>
          <pre>
            {result?.receipt
              ? JSON.stringify(result.receipt, null, 2)
              : "Run code to inspect what Rookhold observed."}
          </pre>
        </aside>
      </section>
    </main>
  );
}
