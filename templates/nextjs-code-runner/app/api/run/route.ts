import { Rookhold } from "rookhold";

export async function POST(request: Request) {
  let body: { language?: string; code?: string };
  try {
    body = (await request.json()) as { language?: string; code?: string };
  } catch {
    return Response.json({ error: "Request body must be JSON." }, { status: 400 });
  }
  if (!body.code || body.code.length > 100_000) {
    return Response.json({ error: "Code must contain 1–100,000 characters." }, { status: 400 });
  }
  // Add application authentication, per-user quotas, and abuse controls before production use.
  try {
    const result = await Rookhold.fromEnv().run({
      language: body.language ?? "python",
      code: body.code,
      limits: { wall_seconds: 3, mem_mb: 128 },
      requirements: { minimum_isolation: "gvisor-application-kernel" },
    });
    return Response.json({
      jobId: result.jobId,
      status: result.status,
      stdout: result.stdout,
      stderr: result.stderr,
      durationMs: result.durationMs,
      isolation: result.isolation,
      receipt: result.receipt,
    });
  } catch {
    return Response.json(
      { error: "Could not run the job. Check the Rookhold URL, key, and isolation." },
      { status: 502 },
    );
  }
}
