"""Static consumer contract; checked by Pyright and Mypy, not executed."""

from typing import Iterator, Optional, cast

from coop import (
    Coop,
    CoopEvent,
    EffectiveJobSpec,
    ExecutorOutputEvidence,
    HashedCoopEvent,
    JobDetail,
    JobPage,
    JobResult,
    Limits,
    OutputEvidence,
    Receipt,
    SubmitResponse,
)

client = Coop("https://coop.example", "tenant-key", timeout=5)
submitted: SubmitResponse = client.submit(
    "python",
    "print(42)",
    limits=Limits(wall_seconds=5, mem_mb=128),
)
page: JobPage = client.list(status="running", language="python")
detail: JobDetail = client.get(submitted["job_id"])
receipt: Optional[Receipt] = detail["receipt"]
terminal: JobDetail = client.wait(submitted["job_id"])
events: Iterator[CoopEvent] = client.stream(submitted["job_id"])
result: JobResult = client.result(submitted["job_id"])
hashed: HashedCoopEvent = cast(HashedCoopEvent, next(events))

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
    hashed,
)
