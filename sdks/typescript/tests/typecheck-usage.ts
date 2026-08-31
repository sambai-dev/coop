/** Static consumer contract; checked by `npm run typecheck`, not executed. */
import type {
  ArtifactDownload,
  AttestationCapabilities,
  AttestationPublicKey,
  CancellationResult,
  Capabilities,
  RookholdEvent,
  ExecutorOutputEvidence,
  HashedRookholdEvent,
  JobDetail,
  JobAttestationStatus,
  OutputEvidence,
  Receipt,
  EffectiveJobSpec,
  IsolationClass,
  SubmitResult,
  WhoAmI,
} from "../rookhold.js";
import { Rookhold, RookholdError, isolationSatisfies } from "../rookhold.js";
import { Coop, CoopError } from "../coop.js";

const client = new Rookhold("https://rookhold.example", "tenant-key", { timeoutMs: 5_000 });
const legacyClient: Coop = new Coop("https://rookhold.example", "tenant-key");
const legacyError: CoopError = new CoopError("compatibility");
void legacyClient;
void legacyError;

async function consume(): Promise<void> {
  const submitted = await client.submit("python", "print(42)", {
    limits: { wall_seconds: 5, mem_mb: 128 },
    requirements: { minimum_isolation: "linux-shared-kernel" },
    idempotencyKey: "consumer-request-1",
    retryAmbiguous: true,
  });
  const submittedWithHeaders: SubmitResult = await client.submitResult(
    "python",
    "print(42)",
    { idempotencyKey: "consumer-request-2" },
  );
  const detail: JobDetail = await client.get(submitted.job_id);
  const terminal: JobDetail = await client.wait(submitted.job_id);
  const effective: EffectiveJobSpec | null = detail.effective_spec;
  const receipt: Receipt | null = terminal.receipt;
  const executorOutput: ExecutorOutputEvidence | null | undefined =
    receipt?.executor_output;
  const durableOutput: OutputEvidence | undefined = receipt?.output;
  const events: RookholdEvent[] = await client.replay(submitted.job_id);
  const cancellation: CancellationResult = await client.cancelResult(submitted.job_id);
  const cancelledJob: JobDetail["job_id"] | undefined = cancellation.job?.job_id;
  const identity: WhoAmI = await client.whoami();
  const capabilities: Capabilities = await client.capabilities();
  const attestationCapabilities: AttestationCapabilities = capabilities.attestations;
  const attestationStatus: JobAttestationStatus = detail.attestation;
  const attestationTenant: string | null = attestationStatus.tenant;
  const publicKey: AttestationPublicKey = await client.attestationPublicKey();
  const envelope: ArtifactDownload = await client.downloadAttestation(
    submitted.job_id,
  );
  const resultArtifact: ArtifactDownload = await client.downloadResultArtifact(
    submitted.job_id,
    { timeoutMs: 10_000 },
  );
  const observed: IsolationClass = capabilities.execution.isolation_class;
  const postureSatisfied: boolean = isolationSatisfies(
    observed,
    "linux-shared-kernel",
  );
  const concurrentMemory: number = capabilities.limits.concurrent_mem_mb_max;
  if (
    events[0]?.hash_version === 1 &&
    typeof events[0].event_hash === "string"
  ) {
    const hashed = events[0] as HashedRookholdEvent;
    void hashed.event_hash;
  }
  const cancelled: void = await client.cancel(submitted.job_id);
  void [
    effective,
    receipt,
    durableOutput,
    executorOutput,
    events,
    identity,
    submittedWithHeaders,
    postureSatisfied,
    concurrentMemory,
    cancelledJob,
    cancelled,
    attestationCapabilities,
    attestationStatus,
    attestationTenant,
    publicKey,
    envelope,
    resultArtifact,
  ];
}

function preserveAmbiguousSubmission(error: unknown): string | undefined {
  return error instanceof RookholdError ? error.idempotencyKey : undefined;
}

void [consume, preserveAmbiguousSubmission];
