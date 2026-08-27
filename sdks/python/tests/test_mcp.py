import io
import json
import os
import unittest
from unittest.mock import patch

from coop_mcp import CoopMcpServer, McpConfig, serve


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
            "cpu_seconds_max": 300,
            "mem_mb_max": 4096,
            "pids_max": 1024,
            "file_mb_max": 1024,
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
        self.result_called = False

    def capabilities(self):
        return self.posture

    def submit(self, language, code, stdin=None, limits=None):
        self.submissions.append((language, code, stdin, limits))
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
    server.handle({"jsonrpc": "2.0", "method": "notifications/initialized"})
    return server


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

    def test_wait_timeout_returns_a_resumable_job_instead_of_losing_it(self):
        result = tool_call(
            initialized_server(FakeCoop(result_error=TimeoutError("late"))),
            "coop_run_code",
            {"language": "python", "code": "while True: pass", "wait_seconds": 1},
        )
        self.assertFalse(result["isError"])
        self.assertEqual(result["structuredContent"]["job_id"], "job-1")
        self.assertFalse(result["structuredContent"]["complete"])

    def test_operator_policy_rejects_unisolated_execution(self):
        result = tool_call(
            initialized_server(FakeCoop(isolated=False), require_isolation=True),
            "coop_run_code",
            {"language": "python", "code": "print(1)"},
        )
        self.assertTrue(result["isError"])
        self.assertIn(
            "requires the isolated namespace backend", result["content"][0]["text"]
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

    def test_stdio_transport_is_newline_delimited_json_rpc(self):
        source = io.StringIO(
            '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}\n{not-json}\n'
        )
        output = io.StringIO()
        serve(initialized_server(), source, output)
        messages = [json.loads(line) for line in output.getvalue().splitlines()]
        self.assertEqual(messages[0]["id"], 1)
        self.assertEqual(messages[1]["error"]["code"], -32700)

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
            },
            clear=True,
        ):
            config = McpConfig.from_env()
        self.assertEqual(config.allowed_languages, frozenset({"python"}))
        self.assertTrue(config.require_isolation)

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
