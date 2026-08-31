"""Live Python SDK and MCP adapter verification against a real Rookhold server."""

from __future__ import annotations

import os
import uuid
from typing import cast

from rookhold import IsolationClass, Rookhold, RookholdError, isolation_satisfies
from rookhold_mcp import McpConfig, RookholdMcpServer


def require(condition: bool, message: str) -> None:
    if not condition:
        raise AssertionError(message)


def main() -> None:
    base_url = os.environ["ROOKHOLD_VERIFY_BASE_URL"]
    api_key = os.environ["ROOKHOLD_CLIENT_KEY"]
    minimum = cast(
        IsolationClass,
        os.environ.get("ROOKHOLD_VERIFY_MINIMUM_ISOLATION", "linux-shared-kernel"),
    )
    client = Rookhold(base_url, api_key, timeout=10)

    capabilities = client.capabilities()
    observed = capabilities["execution"]["isolation_class"]
    require(
        isolation_satisfies(observed, minimum),
        f"live provider {observed!r} did not satisfy {minimum!r}",
    )
    require(
        capabilities["limits"]["concurrent_mem_mb_max"]
        >= capabilities["limits"]["mem_mb_max"],
        "concurrent memory capability was below the per-job maximum",
    )

    key = f"python-live-{uuid.uuid4()}"
    requirements = {"minimum_isolation": minimum}
    first = client.submit_result(
        "python",
        "print('python-sdk-live')",
        requirements=requirements,
        idempotency_key=key,
    )
    replay = client.submit_result(
        "python",
        "print('python-sdk-live')",
        requirements=requirements,
        idempotency_key=key,
    )
    require(not first["idempotency_replayed"], "first keyed submit was a replay")
    require(replay["idempotency_replayed"], "second keyed submit was not a replay")
    require(first["job"]["job_id"] == replay["job"]["job_id"], "replay changed job")
    require(first["location"] == replay["location"], "replay changed Location")
    job_id = first["job"]["job_id"]
    result = client.result(job_id, timeout=60)
    require(result["status"] == "succeeded", f"live SDK job failed: {result}")
    detail = client.get(job_id)
    effective = detail["effective_spec"]
    require(effective is not None, "terminal job omitted effective_spec")
    effective_class = effective["isolation_class"]
    require(
        effective_class is not None, "terminal job omitted observed isolation_class"
    )
    require(
        isolation_satisfies(effective_class, minimum),
        f"terminal isolation {effective_class!r} did not satisfy the requirement",
    )

    cancellable = client.submit(
        "python",
        "import time; time.sleep(60)",
        requirements=requirements,
    )
    cancellation = client.cancel_result(cancellable["job_id"])
    require(cancellation["cancellation_requested"], "cancel was not accepted")
    require(not cancellation["already_terminal"], "new job was already terminal")
    client.wait(cancellable["job_id"], timeout=30, poll_interval=0.1)
    terminal_cancel = client.cancel_result(cancellable["job_id"])
    require(
        not terminal_cancel["cancellation_requested"], "terminal cancel was reissued"
    )
    require(terminal_cancel["already_terminal"], "terminal cancellation was not typed")

    stronger_minimum: IsolationClass = "gvisor-application-kernel"
    if isolation_satisfies(observed, stronger_minimum):
        stronger = client.submit(
            "python",
            "print('stronger-minimum-live')",
            requirements={"minimum_isolation": stronger_minimum},
        )
        stronger_result = client.result(stronger["job_id"], timeout=60)
        require(stronger_result["status"] == "succeeded", "stronger minimum failed")
    else:
        try:
            client.submit(
                "python",
                "pass",
                requirements={"minimum_isolation": stronger_minimum},
            )
        except RookholdError as exc:
            require(
                exc.code == "minimum_isolation_unsatisfied",
                f"unexpected minimum-isolation error: {exc}",
            )
        else:
            raise AssertionError("provider accepted an unsatisfied stronger minimum")

    adapter = RookholdMcpServer(
        McpConfig(
            base_url=base_url,
            api_key=api_key,
            allowed_languages=["python"],
            max_wait_seconds=60,
            max_code_bytes=1024,
            require_isolation=False,
            minimum_isolation=minimum,
        ),
        client=client,
    )
    adapter.handle(
        {
            "jsonrpc": "2.0",
            "id": "init",
            "method": "initialize",
            "params": {"protocolVersion": "2025-11-25"},
        }
    )
    adapter.handle({"jsonrpc": "2.0", "method": "notifications/initialized"})
    adapter_result = adapter.handle(
        {
            "jsonrpc": "2.0",
            "id": "run",
            "method": "tools/call",
            "params": {
                "name": "rookhold_run_code",
                "arguments": {"language": "python", "code": "print('mcp-live')"},
            },
        }
    )
    require(adapter_result is not None, "adapter returned no response")
    require("error" not in adapter_result, f"adapter protocol error: {adapter_result}")
    tool_result = adapter_result["result"]
    require(not tool_result["isError"], f"adapter tool failed: {tool_result}")
    require(
        isolation_satisfies(
            tool_result["structuredContent"]["effective_spec"]["isolation_class"],
            minimum,
        ),
        "adapter did not return observed terminal isolation",
    )

    adapter_cancellable = client.submit(
        "python",
        "import time; time.sleep(60)",
        requirements=requirements,
    )
    adapter_cancel = adapter.handle(
        {
            "jsonrpc": "2.0",
            "id": "cancel",
            "method": "tools/call",
            "params": {
                "name": "rookhold_cancel_job",
                "arguments": {"job_id": adapter_cancellable["job_id"]},
            },
        }
    )
    require(adapter_cancel is not None, "adapter cancel returned no response")
    cancel_content = adapter_cancel["result"]["structuredContent"]
    require(cancel_content["cancellation_requested"], "adapter lost typed cancel state")
    require(
        not cancel_content["already_terminal"], "adapter cancel raced terminal state"
    )


if __name__ == "__main__":
    main()
