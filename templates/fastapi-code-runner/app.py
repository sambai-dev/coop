from typing import Literal

from fastapi import FastAPI, HTTPException
from pydantic import BaseModel, Field
from rookhold import Limits, Rookhold

app = FastAPI(title="Rookhold user-code example")
rookhold = Rookhold.from_env()


class RunRequest(BaseModel):
    language: Literal["python", "node", "bash"] = "python"
    code: str = Field(min_length=1, max_length=100_000)
    stdin: str | None = Field(default=None, max_length=100_000)


@app.post("/run")
def run_code(request: RunRequest) -> dict[str, object]:
    # Add application authentication, per-user quotas, and abuse controls
    # before exposing this endpoint outside a trusted local environment.
    result = rookhold.run(
        request.language,
        request.code,
        request.stdin,
        limits=Limits(wall_seconds=3, mem_mb=128),
        requirements={"minimum_isolation": "gvisor-application-kernel"},
    )
    if result.status == "error":
        raise HTTPException(502, "Rookhold could not execute the job")
    return {
        "job_id": result.job_id,
        "status": result.status,
        "stdout": result.stdout,
        "stderr": result.stderr,
        "isolation": result.isolation,
        "receipt": result.receipt,
    }
