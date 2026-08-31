"""Static consumer contract; checked by Pyright and Mypy, not executed."""

from typing import Iterator, Optional, cast

from coop import Coop, CoopError
from rookhold import (
    ArtifactDownload,
    AttestationCapabilities,
    AttestationPublicKey,
    CancellationResponse,
    EffectiveJobSpec,
    ExecutionRequirements,
    ExecutorOutputEvidence,
    HashedRookholdEvent,
    IsolationClass,
    JobAttestationStatus,
    JobDetail,
    JobPage,
    JobResult,
    Limits,
    OutputEvidence,
    Receipt,
    Rookhold,
    RookholdEvent,
    RunResult,
    SubmitResponse,
    SubmitResult,
    isolation_satisfies,
)
from rookhold_cli import CliConfig, RookholdCli

client = Rookhold("https://rookhold.example", "tenant-key", timeout=5)
cli_config = CliConfig(
    base_url="https://rookhold.example",
    api_key="tenant-key",
    minimum_isolation="gvisor-application-kernel",
    wait_seconds=60,
    color=False,
    json_output=True,
)
cli_client = RookholdCli(cli_config, client=client)
legacy_client: Coop = Coop("https://rookhold.example", "tenant-key", timeout=5)
legacy_error: CoopError = CoopError("compatibility")
_legacy = (legacy_client, legacy_error)
requirements: ExecutionRequirements = {"minimum_isolation": "linux-shared-kernel"}
submitted: SubmitResponse = client.submit(
    "python",
    "print(42)",
    limits=Limits(wall_seconds=5, mem_mb=128),
    requirements=requirements,
    idempotency_key="consumer-submit-1",
)
submitted_with_metadata: SubmitResult = client.submit_result(
    "python",
    "print(42)",
    requirements=requirements,
    idempotency_key="consumer-submit-2",
)
page: JobPage = client.list(status="running", language="python")
detail: JobDetail = client.get(submitted["job_id"])
attestation_status: JobAttestationStatus = detail["attestation"]
attestation_tenant: Optional[str] = attestation_status["tenant"]
attestation_capabilities: AttestationCapabilities = client.capabilities()[
    "attestations"
]
public_key: AttestationPublicKey = client.attestation_public_key()
envelope: ArtifactDownload = client.download_attestation(submitted["job_id"])
result_artifact: ArtifactDownload = client.download_result_artifact(
    submitted["job_id"], timeout=10
)
receipt: Optional[Receipt] = detail["receipt"]
terminal: JobDetail = client.wait(submitted["job_id"])
events: Iterator[RookholdEvent] = client.stream(submitted["job_id"])
result: JobResult = client.result(submitted["job_id"])
run_result: RunResult = client.run("python", "print(42)")
cancelled: CancellationResponse = client.cancel_result(submitted["job_id"])
hashed: HashedRookholdEvent = cast(HashedRookholdEvent, next(events))
observed_isolation: IsolationClass = "gvisor-application-kernel"
isolation_ok: bool = isolation_satisfies(
    observed_isolation, requirements.get("minimum_isolation", "none")
)

effective: Optional[EffectiveJobSpec] = detail["effective_spec"]
executor_output: Optional[ExecutorOutputEvidence] = (
    receipt.get("executor_output") if receipt is not None else None
)
durable_output: Optional[OutputEvidence] = (
    receipt.get("output") if receipt is not None else None
)

_ = (
    page,
    detail,
    effective,
    receipt,
    durable_output,
    executor_output,
    terminal,
    events,
    result,
    run_result,
    cancelled,
    hashed,
    submitted_with_metadata,
    isolation_ok,
    attestation_status,
    attestation_tenant,
    attestation_capabilities,
    public_key,
    envelope,
    result_artifact,
    cli_client,
)
