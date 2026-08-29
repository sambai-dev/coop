import io
import json
import os
import queue
import threading
import unittest
from unittest.mock import patch

from coop import CoopError
from coop_mcp import (
    MODERN_PROTOCOL_VERSION,
    TASKS_EXTENSION,
    CoopMcpServer,
    McpConfig,
    serve,
)


def capabilities(isolated=True):
    return {
        "version": "0.2.0",
        "languages": ["python", "node", "bash"],
        "execution": {
            "backend": "namespaces+cgroups-v2+private-rootfs"
            if isolated
            else "subprocess",
            "isolated": isolated,
            "private_rootfs": isolated,
            "dedicated_bootstrap": isolated,
            "seccomp": isolated,
            "networking": "disabled" if isolated else "host",
            "limit_enforcement": {
                "wall_seconds": True,
                "cpu_seconds": isolated,
                "mem_mb": isolated,
                "max_pids": isolated,
                "max_file_mb": isolated,
            },
        },
        "limits": {
            "wall_seconds_max": 300,
            "cpu_seconds_max": 240,
            "mem_mb_max": 4096,
            "pids_max": 1024,
            "file_mb_max": 512,
            "output_lines_max": 10000,
            "output_bytes_per_stream_max": 1048576,
            "output_record_bytes_max": 65536,
            "code_bytes_max": 1048576,
            "stdin_bytes_max": 1048576,
        },
        "features": {
            "result_wait": True,
            "cancellation": True,
            "event_cursors": True,
            "stream_tickets": True,
            "receipts": True,
        },
    }


class FakeCoop:
    def __init__(self, *, isolated=True, result_error=None):
        self.posture = capabilities(isolated)
        self.result_error = result_error
        self.submissions = []
        self.requirements = []
        self.result_called = False

    def capabilities(self):
        return self.posture

    def submit(self, language, code, stdin=None, limits=None, requirements=None):
        if requirements is not None and not self.posture["execution"]["isolated"]:
            raise ValueError("minimum isolation requirement was not satisfied")
        self.submissions.append((language, code, stdin, limits))
        self.requirements.append(requirements)
        return {
            "job_id": "job-1",
            "status": "queued",
            "stream_url": "/stream",
            "replay_url": "/replay",
        }

    def result(self, job_id, timeout=60):
        self.result_called = True
        if self.result_error is not None:
            raise self.result_error
        return {
            "job_id": job_id,
            "status": "succeeded",
            "exit_code": 0,
            "stdout": "42",
            "stderr": "",
            "truncated": False,
            "violations": [],
        }

    def get(self, job_id):
        if isinstance(self.result_error, TimeoutError) or not self.result_called:
            return {"job_id": job_id, "status": "running"}
        return {
            "job_id": job_id,
            "status": "succeeded",
            "effective_spec": {"language": "python"},
            "execution_policy": {"isolated": True},
            "receipt": {"bootstrap_ready": True},
            "receipt_sha256": "a" * 64,
        }

    def event_page(self, job_id, *, after=None, limit=500):
        return {
            "events": [{"job_id": job_id, "seq": 1, "kind": "started"}],
            "next_cursor": None,
        }

    def cancel(self, job_id):
        return {"job_id": job_id, "status": "cancelled"}


def initialized_server(fake=None, **config_overrides):
    values = {
        "base_url": "https://coop.example.test",
        "api_key": "super-secret-key",
        "allowed_languages": ["python", "node", "bash"],
        "max_wait_seconds": 300,
        "max_code_bytes": 1024,
        "require_isolation": False,
    }
    values.update(config_overrides)
    server = CoopMcpServer(
        McpConfig(**values),
        client=fake or FakeCoop(),  # type: ignore[arg-type]
    )
    server.handle(
        {
            "jsonrpc": "2.0",
            "id": "init",
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "test", "version": "1"},
            },
        }
    )
    server.handle({"jsonrpc": "2.0", "method": "notifications/initialized"})
    return server


def modern_meta(*, tasks=False):
    capabilities_value = {}
    if tasks:
        capabilities_value["extensions"] = {TASKS_EXTENSION: {}}
    return {
        "io.modelcontextprotocol/protocolVersion": MODERN_PROTOCOL_VERSION,
        "io.modelcontextprotocol/clientCapabilities": capabilities_value,
        "io.modelcontextprotocol/clientInfo": {"name": "test", "version": "1"},
    }


class QueueInput:
    def __init__(self):
        self.values = queue.Queue()

    def put(self, message):
        self.values.put(json.dumps(message) + "\n")

    def close(self):
        self.values.put(None)

    def __iter__(self):
        return self

    def __next__(self):
        value = self.values.get(timeout=5)
        if value is None:
            raise StopIteration
        return value


class ConcurrentOutput:
    def __init__(self):
        self._condition = threading.Condition()
        self._lines = []

    def write(self, value):
        with self._condition:
            self._lines.extend(line for line in value.splitlines() if line)
            self._condition.notify_all()
        return len(value)

    def flush(self):
        return None

    def messages(self):
        with self._condition:
            return [json.loads(line) for line in self._lines]

    def wait_for_id(self, request_id, timeout=3):
        def found():
            return any(message.get("id") == request_id for message in self.messages())

        with self._condition:
            if not self._condition.wait_for(found, timeout=timeout):
                raise AssertionError(f"response {request_id!r} was not written")
        return next(
            message for message in self.messages() if message.get("id") == request_id
        )


class BlockingFakeCoop(FakeCoop):
    def __init__(self):
        super().__init__()
        self.result_started = threading.Event()
        self.release_result = threading.Event()
        self.cancel_seen = threading.Event()

    def result(self, job_id, timeout=60):
        self.result_called = True
        self.result_started.set()
        if not self.release_result.wait(timeout=3):
            raise TimeoutError("test result remained blocked")
        return super().result(job_id, timeout=timeout)

    def cancel(self, job_id):
        self.cancel_seen.set()
        self.release_result.set()
        return None


def tool_call(server, name, arguments):
    response = server.handle(
        {
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments},
        }
    )
    return response["result"]


class McpTests(unittest.TestCase):
    def test_initialize_and_list_tools_without_exposing_credentials(self):
        server = initialized_server()
        response = server.handle(
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": {"name": "test", "version": "1"},
                },
            }
        )
        self.assertEqual(response["result"]["protocolVersion"], "2025-11-25")
        listed = server.handle(
            {"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}
        )
        names = [tool["name"] for tool in listed["result"]["tools"]]
        self.assertEqual(
            names,
            [
                "coop_run_code",
                "coop_job_result",
                "coop_job_events",
                "coop_cancel_job",
            ],
        )
        self.assertNotIn("super-secret-key", json.dumps(listed))
        self.assertNotIn("base_url", json.dumps(listed))

    def test_modern_discovery_is_stateless_and_tasks_are_modern_only(self):
        server = CoopMcpServer(
            McpConfig(
                base_url="https://coop.example.test",
                api_key="super-secret-key",
                allowed_languages=["python"],
                max_wait_seconds=300,
                max_code_bytes=1024,
                require_isolation=False,
                enable_tasks=True,
            ),
            client=FakeCoop(),  # type: ignore[arg-type]
        )
        discovered = server.handle(
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": "server/discover",
                "params": {"_meta": modern_meta(tasks=True)},
            }
        )
        self.assertEqual(discovered["result"]["resultType"], "complete")
        self.assertEqual(
            discovered["result"]["supportedVersions"], [MODERN_PROTOCOL_VERSION]
        )
        self.assertEqual(
            discovered["result"]["capabilities"]["extensions"],
            {TASKS_EXTENSION: {}},
        )
        self.assertIn(
            "io.modelcontextprotocol/serverInfo", discovered["result"]["_meta"]
        )
        legacy = server.handle(
            {
                "jsonrpc": "2.0",
                "id": 2,
                "method": "initialize",
                "params": {"protocolVersion": "2025-11-25"},
            }
        )
        self.assertNotIn("extensions", legacy["result"]["capabilities"])
        unsupported = server.handle(
            {
                "jsonrpc": "2.0",
                "id": 3,
                "method": "server/discover",
                "params": {
                    "_meta": {
                        "io.modelcontextprotocol/protocolVersion": "2099-01-01",
                        "io.modelcontextprotocol/clientCapabilities": {},
                    }
                },
            }
        )
        self.assertEqual(unsupported["error"]["code"], -32022)
        self.assertEqual(
            unsupported["error"]["data"]["supported"], [MODERN_PROTOCOL_VERSION]
        )
        modern_ping = server.handle(
            {
                "jsonrpc": "2.0",
                "id": 4,
                "method": "ping",
                "params": {"_meta": modern_meta()},
            }
        )
        self.assertEqual(modern_ping["error"]["code"], -32601)

    def test_modern_tool_list_uses_live_capabilities_and_complete_results(self):
        server = initialized_server(enable_tasks=True)
        listed = server.handle(
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/list",
                "params": {"_meta": modern_meta(tasks=True)},
            }
        )
        result = listed["result"]
        self.assertEqual(result["resultType"], "complete")
        self.assertEqual(result["ttlMs"], 0)
        self.assertEqual(result["cacheScope"], "private")
        run = result["tools"][0]
        limits = run["inputSchema"]["properties"]["limits"]["properties"]
        self.assertEqual(limits["cpu_seconds"]["maximum"], 240)
        self.assertEqual(limits["max_file_mb"]["maximum"], 512)
        self.assertEqual(limits["max_pids"]["minimum"], 8)
        self.assertFalse(limits["allow_network"]["const"])
        self.assertEqual(run["execution"]["taskSupport"], "optional")
        self.assertIn("outputSchema", run)

        modern_call = server.handle(
            {
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "coop_job_result",
                    "arguments": {"job_id": "job-1", "wait_seconds": 0},
                    "_meta": modern_meta(),
                },
            }
        )
        self.assertEqual(modern_call["result"]["resultType"], "complete")
        self.assertIn(
            "io.modelcontextprotocol/serverInfo", modern_call["result"]["_meta"]
        )

    def test_unisolated_tool_annotations_are_conservative(self):
        server = initialized_server(FakeCoop(isolated=False))
        listed = server.handle(
            {"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}}
        )
        annotations = listed["result"]["tools"][0]["annotations"]
        self.assertTrue(annotations["destructiveHint"])
        self.assertTrue(annotations["openWorldHint"])

    def test_run_code_returns_structured_result_and_job_id(self):
        fake = FakeCoop()
        result = tool_call(
            initialized_server(fake),
            "coop_run_code",
            {
                "language": "python",
                "code": "print(6 * 7)",
                "limits": {"wall_seconds": 5},
            },
        )
        self.assertFalse(result["isError"])
        self.assertEqual(result["structuredContent"]["job_id"], "job-1")
        self.assertEqual(result["structuredContent"]["stdout"], "42")
        self.assertTrue(result["structuredContent"]["complete"])
        self.assertEqual(result["structuredContent"]["receipt_sha256"], "a" * 64)
        self.assertTrue(result["structuredContent"]["receipt"]["bootstrap_ready"])
        self.assertEqual(
            fake.submissions,
            [("python", "print(6 * 7)", None, {"wall_seconds": 5})],
        )

    def test_tasks_map_durably_to_coop_job_ids_when_both_sides_opt_in(self):
        fake = FakeCoop()
        server = initialized_server(fake, enable_tasks=True)
        created = server.handle(
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": "coop_run_code",
                    "arguments": {"language": "python", "code": "print(42)"},
                    "_meta": modern_meta(tasks=True),
                },
            }
        )["result"]
        self.assertEqual(created["resultType"], "task")
        self.assertEqual(created["taskId"], "job-1")
        self.assertEqual(created["status"], "working")
        self.assertIsInstance(created["ttlMs"], int)

        working = server.handle(
            {
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tasks/get",
                "params": {"taskId": "job-1", "_meta": modern_meta(tasks=True)},
            }
        )["result"]
        self.assertEqual(working["status"], "working")

        fake.result_called = True
        completed = server.handle(
            {
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tasks/get",
                "params": {"taskId": "job-1", "_meta": modern_meta(tasks=True)},
            }
        )["result"]
        self.assertEqual(completed["status"], "completed")
        self.assertEqual(completed["result"]["resultType"], "complete")
        self.assertEqual(completed["result"]["structuredContent"]["job_id"], "job-1")
        updated = server.handle(
            {
                "jsonrpc": "2.0",
                "id": 4,
                "method": "tasks/update",
                "params": {
                    "taskId": "job-1",
                    "inputResponses": {"unused": {}},
                    "_meta": modern_meta(tasks=True),
                },
            }
        )
        self.assertEqual(updated["result"]["resultType"], "complete")
        cancelled = server.handle(
            {
                "jsonrpc": "2.0",
                "id": 5,
                "method": "tasks/cancel",
                "params": {"taskId": "job-1", "_meta": modern_meta(tasks=True)},
            }
        )
        self.assertEqual(cancelled["result"]["resultType"], "complete")

    def test_task_methods_require_per_request_extension_capability(self):
        server = initialized_server(enable_tasks=True)
        denied = server.handle(
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tasks/get",
                "params": {"taskId": "job-1", "_meta": modern_meta()},
            }
        )
        self.assertEqual(denied["error"]["code"], -32021)

        class MissingTaskFake(FakeCoop):
            def get(self, job_id):
                raise CoopError("job not found", status=404, code="job_not_found")

        missing = initialized_server(MissingTaskFake(), enable_tasks=True).handle(
            {
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tasks/get",
                "params": {"taskId": "missing", "_meta": modern_meta(tasks=True)},
            }
        )
        self.assertEqual(missing["error"]["code"], -32602)

    def test_wait_timeout_returns_a_resumable_job_instead_of_losing_it(self):
        result = tool_call(
            initialized_server(FakeCoop(result_error=TimeoutError("late"))),
            "coop_run_code",
            {"language": "python", "code": "while True: pass", "wait_seconds": 1},
        )
        self.assertFalse(result["isError"])
        self.assertEqual(result["structuredContent"]["job_id"], "job-1")
        self.assertFalse(result["structuredContent"]["complete"])

    def test_transport_timeout_after_submit_preserves_the_durable_job_id(self):
        result = tool_call(
            initialized_server(
                FakeCoop(
                    result_error=CoopError(
                        "socket wait expired",
                        code="request_timeout",
                        retryable=True,
                    )
                )
            ),
            "coop_run_code",
            {"language": "python", "code": "pass", "wait_seconds": 1},
        )
        self.assertFalse(result["isError"])
        self.assertEqual(result["structuredContent"]["job_id"], "job-1")
        self.assertFalse(result["structuredContent"]["complete"])
        self.assertEqual(result["structuredContent"]["error_code"], "request_timeout")

    def test_operator_policy_rejects_unisolated_execution(self):
        result = tool_call(
            initialized_server(FakeCoop(isolated=False), require_isolation=True),
            "coop_run_code",
            {"language": "python", "code": "print(1)"},
        )
        self.assertTrue(result["isError"])
        self.assertIn("minimum isolation", result["content"][0]["text"])

        isolated = FakeCoop(isolated=True)
        accepted = tool_call(
            initialized_server(isolated, require_isolation=True),
            "coop_run_code",
            {"language": "python", "code": "print(1)"},
        )
        self.assertFalse(accepted["isError"])
        self.assertEqual(
            isolated.requirements,
            [{"minimum_isolation": "linux-shared-kernel"}],
        )

    def test_language_code_size_and_unknown_arguments_are_adapter_policy(self):
        server = initialized_server(
            allowed_languages=["python"], max_code_bytes=4, max_wait_seconds=5
        )
        listed = server.handle(
            {"jsonrpc": "2.0", "id": 3, "method": "tools/list", "params": {}}
        )
        run_schema = listed["result"]["tools"][0]["inputSchema"]
        self.assertEqual(run_schema["properties"]["language"]["enum"], ["python"])
        self.assertEqual(run_schema["properties"]["wait_seconds"]["maximum"], 5)
        denied = tool_call(
            server, "coop_run_code", {"language": "bash", "code": "true"}
        )
        oversized = tool_call(
            server, "coop_run_code", {"language": "python", "code": "12345"}
        )
        unknown = tool_call(
            server,
            "coop_run_code",
            {"language": "python", "code": "1", "base_url": "https://evil.test"},
        )
        self.assertTrue(denied["isError"])
        self.assertTrue(oversized["isError"])
        self.assertTrue(unknown["isError"])

    def test_events_cancel_and_immediate_status(self):
        server = initialized_server()
        status = tool_call(
            server, "coop_job_result", {"job_id": "job-1", "wait_seconds": 0}
        )
        events = tool_call(
            server, "coop_job_events", {"job_id": "job-1", "after": -1, "limit": 10}
        )
        cancelled = tool_call(server, "coop_cancel_job", {"job_id": "job-1"})
        self.assertFalse(status["structuredContent"]["complete"])
        self.assertEqual(events["structuredContent"]["events"][0]["kind"], "started")
        self.assertTrue(cancelled["structuredContent"]["cancelled"])

    def test_empty_cancel_ack_is_reported_as_accepted(self):
        class EmptyCancelFake(FakeCoop):
            def cancel(self, job_id):
                return None

        result = tool_call(
            initialized_server(EmptyCancelFake()),
            "coop_cancel_job",
            {"job_id": "job-1"},
        )
        self.assertTrue(result["structuredContent"]["cancelled"])
        self.assertIsNone(result["structuredContent"]["job"])

    def test_invalid_ids_and_unknown_tools_are_protocol_errors(self):
        server = initialized_server()
        invalid = server.handle({"jsonrpc": "2.0", "id": True, "method": "ping"})
        self.assertEqual(invalid["error"]["code"], -32600)
        unknown = server.handle(
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {"name": "missing", "arguments": {}},
            }
        )
        self.assertEqual(unknown["error"]["code"], -32602)

    def test_stdio_transport_is_newline_delimited_json_rpc(self):
        source = io.StringIO(
            '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}\n{not-json}\n'
        )
        output = io.StringIO()
        serve(initialized_server(), source, output)
        messages = [json.loads(line) for line in output.getvalue().splitlines()]
        self.assertEqual(messages[0]["id"], 1)
        self.assertEqual(messages[1]["error"]["code"], -32700)

    def test_stdio_ping_is_not_blocked_by_a_long_tool_request(self):
        fake = BlockingFakeCoop()
        source = QueueInput()
        output = ConcurrentOutput()
        worker = threading.Thread(
            target=serve,
            args=(initialized_server(fake), source, output),
            daemon=True,
        )
        worker.start()
        source.put(
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": "coop_run_code",
                    "arguments": {"language": "python", "code": "pass"},
                },
            }
        )
        self.assertTrue(fake.result_started.wait(timeout=2))
        source.put({"jsonrpc": "2.0", "id": 2, "method": "ping"})
        self.assertEqual(output.wait_for_id(2)["result"], {})
        self.assertFalse(any(message.get("id") == 1 for message in output.messages()))
        fake.release_result.set()
        self.assertEqual(
            output.wait_for_id(1)["result"]["structuredContent"]["job_id"], "job-1"
        )
        source.close()
        worker.join(timeout=3)
        self.assertFalse(worker.is_alive())

    def test_stdio_request_cancellation_cancels_owned_job_and_suppresses_response(self):
        fake = BlockingFakeCoop()
        source = QueueInput()
        output = ConcurrentOutput()
        server = initialized_server(fake)
        worker = threading.Thread(
            target=serve,
            args=(server, source, output),
            daemon=True,
        )
        worker.start()
        source.put(
            {
                "jsonrpc": "2.0",
                "id": "run",
                "method": "tools/call",
                "params": {
                    "name": "coop_run_code",
                    "arguments": {"language": "python", "code": "pass"},
                },
            }
        )
        self.assertTrue(fake.result_started.wait(timeout=2))
        source.put(
            {
                "jsonrpc": "2.0",
                "method": "notifications/cancelled",
                "params": {"requestId": "run", "reason": "user stopped"},
            }
        )
        self.assertTrue(fake.cancel_seen.wait(timeout=2))
        source.put({"jsonrpc": "2.0", "id": "ping", "method": "ping"})
        output.wait_for_id("ping")
        source.close()
        worker.join(timeout=3)
        self.assertFalse(worker.is_alive())
        self.assertFalse(
            any(message.get("id") == "run" for message in output.messages())
        )

    def test_environment_config_requires_explicit_key(self):
        with patch.dict(os.environ, {}, clear=True):
            with self.assertRaisesRegex(ValueError, "COOP_API_KEY is required"):
                McpConfig.from_env()
        with patch.dict(
            os.environ,
            {
                "COOP_API_KEY": "key",
                "COOP_MCP_ALLOWED_LANGUAGES": "python",
                "COOP_MCP_REQUIRE_ISOLATION": "true",
                "COOP_MCP_ENABLE_TASKS": "true",
                "COOP_MCP_TASK_TTL_MS": "600000",
            },
            clear=True,
        ):
            config = McpConfig.from_env()
        self.assertEqual(config.allowed_languages, frozenset({"python"}))
        self.assertTrue(config.require_isolation)
        self.assertTrue(config.enable_tasks)
        self.assertEqual(config.task_ttl_ms, 600000)

    def test_errors_redact_the_tenant_key(self):
        class LeakyFake(FakeCoop):
            def capabilities(self):
                raise ValueError("request failed with super-secret-key")

        result = tool_call(
            initialized_server(LeakyFake()),
            "coop_run_code",
            {"language": "python", "code": "print(1)"},
        )
        serialized = json.dumps(result)
        self.assertNotIn("super-secret-key", serialized)
        self.assertIn("[REDACTED]", serialized)


if __name__ == "__main__":
    unittest.main()
