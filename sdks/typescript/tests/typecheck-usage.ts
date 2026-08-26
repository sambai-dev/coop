/** Static consumer contract; checked by `npm run typecheck`, not executed. */
import type {
  CoopEvent,
  ExecutorOutputEvidence,
  HashedCoopEvent,
  JobDetail,
  OutputEvidence,
  Receipt,
  EffectiveJobSpec,
} from "../coop.js";
import { Coop } from "../coop.js";

const client = new Coop("https://coop.example", "tenant-key", { timeoutMs: 5_000 });

async function consume(): Promise<void> {
  const submitted = await client.submit("python", "print(42)", {
    limits: { wall_seconds: 5, mem_mb: 128 },
  });
  const detail: JobDetail = await client.get(submitted.job_id);
  const terminal: JobDetail = await client.wait(submitted.job_id);
  const effective: EffectiveJobSpec | null = detail.effective_spec;
  const receipt: Receipt | null = terminal.receipt;
  const executorOutput: ExecutorOutputEvidence | null | undefined =
    receipt?.executor_output;
  const durableOutput: OutputEvidence | undefined = receipt?.output;
  const events: CoopEvent[] = await client.replay(submitted.job_id);
  if (
    events[0]?.hash_version === 1 &&
    typeof events[0].event_hash === "string"
  ) {
    const hashed = events[0] as HashedCoopEvent;
    void hashed.event_hash;
  }
  const cancelled: void = await client.cancel(submitted.job_id);
  void [effective, receipt, durableOutput, executorOutput, events, cancelled];
}

void consume;
