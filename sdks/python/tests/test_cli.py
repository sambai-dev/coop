import io
import json
import unittest
from unittest.mock import patch

from rookhold_cli import CliConfig, RookholdCli, _compatible_env, main


class FakeRookhold:
    def __init__(self) -> None:
        self.requirements = None
        self.cancelled = None

    def whoami(self):
        return {
            "tenant": "local",
            "principal_id": "legacy:local",
            "credential_id": None,
            "auth_method": "api_key",
            "scopes": ["jobs:submit", "jobs:read", "jobs:cancel"],
            "expires_at_ms": None,
        }

    def capabilities(self):
        return {
            "version": "0.7.0",
            "languages": ["python", "node", "bash"],
            "execution": {
                "backend": "off",
                "isolation_class": "none",
                "isolated": False,
                "private_rootfs": False,
                "dedicated_bootstrap": False,
                "seccomp": False,
                "networking": "host",
                "limit_enforcement": {
                    "wall_seconds": True,
                    "cpu_seconds": False,
                    "mem_mb": False,
                    "max_pids": False,
                    "max_file_mb": False,
                },
            },
            "limits": {
                "code_bytes_max": 1024 * 1024,
                "stdin_bytes_max": 1024 * 1024,
                "wall_seconds_max": 300,
                "cpu_seconds_max": 120,
                "mem_mb_max": 1024,
                "pids_max": 256,
                "file_mb_max": 64,
                "concurrent_mem_mb_max": 4096,
            },
            "attestations": {"enabled": False},
        }

    def submit_result(self, language, code, *, requirements):
        self.requirements = requirements
        return {
            "job": {
                "job_id": "01a0-demo-job",
                "tenant": "local",
                "language": language,
                "status": "queued",
                "created_at_ms": 1,
            },
            "location": "/v1/jobs/01a0-demo-job",
            "idempotency_replayed": False,
        }

    def result(self, job_id, *, timeout):
        return {
            "job_id": job_id,
            "status": "succeeded",
            "exit_code": 0,
            "stdout": "42",
            "stderr": "",
            "duration_ms": 38,
            "truncated": False,
            "violations": [],
        }

    def list(self, *, limit):
        return {
            "items": [
                {
                    "job_id": "01a0-demo-job",
                    "status": "succeeded",
                    "language": "python",
                }
            ][:limit],
            "next_cursor": None,
        }

    def get(self, job_id):
        return {"job_id": job_id, "status": "succeeded", "tenant": "local"}

    def replay(self, job_id, *, after=0):
        return [
            {
                "job_id": job_id,
                "seq": after + 1,
                "kind": "stdout",
                "data": {"line": "42"},
            }
        ]

    def cancel_result(self, job_id):
        self.cancelled = job_id
        return {
            "job_id": job_id,
            "cancellation_requested": True,
            "already_terminal": False,
            "status": "running",
        }

    def cancel(self, job_id):
        self.cancelled = job_id


def config() -> CliConfig:
    return CliConfig(
        base_url="http://127.0.0.1:7300",
        api_key="secret-never-print",
        minimum_isolation="none",
        wait_seconds=60,
        color=False,
        json_output=False,
    )


class RookholdCliTests(unittest.TestCase):
    def test_banner_reports_live_posture_and_real_mcp_tools_without_key(self):
        output = io.StringIO()
        fake = FakeRookhold()
        cli = RookholdCli(config(), client=fake, output_stream=output)  # type: ignore[arg-type]
        cli.connect()
        cli.banner()
        rendered = output.getvalue()
        self.assertIn("Rookhold CLI", rendered)
        self.assertIn("Runtime    off", rendered)
        self.assertIn("Isolation  none", rendered)
        self.assertIn("MCP        4 tools ready", rendered)
        self.assertNotIn("secret-never-print", rendered)

    def test_run_is_policy_bound_and_prints_bounded_result(self):
        output = io.StringIO()
        fake = FakeRookhold()
        cli = RookholdCli(config(), client=fake, output_stream=output)  # type: ignore[arg-type]
        result = cli.run_code("python", "print(6 * 7)")
        self.assertEqual(fake.requirements, {"minimum_isolation": "none"})
        self.assertEqual(result["stdout"], "42")
        self.assertEqual(cli.last_job_id, "01a0-demo-job")
        self.assertIn("✓ succeeded", output.getvalue())
        self.assertIn("42", output.getvalue())

    def test_scripted_shell_exercises_mcp_and_execution(self):
        output = io.StringIO()
        fake = FakeRookhold()
        cli = RookholdCli(
            config(),
            client=fake,  # type: ignore[arg-type]
            input_stream=io.StringIO('/mcp\n/run python "print(6 * 7)"\n/quit\n'),
            output_stream=output,
        )
        self.assertEqual(cli.shell(), 0)
        rendered = output.getvalue()
        for tool in [
            "rookhold_run_code",
            "rookhold_job_result",
            "rookhold_job_events",
            "rookhold_cancel_job",
        ]:
            self.assertIn(tool, rendered)
        self.assertIn("✓ succeeded", rendered)
        self.assertNotIn("secret-never-print", rendered)

    def test_follow_up_commands_default_to_last_job(self):
        output = io.StringIO()
        fake = FakeRookhold()
        cli = RookholdCli(config(), client=fake, output_stream=output)  # type: ignore[arg-type]
        cli.run_code("python", "print(42)")
        cli.events(None)
        cli.cancel(None)
        self.assertEqual(fake.cancelled, "01a0-demo-job")
        self.assertIn("stdout", output.getvalue())

    def test_environment_aliases_conflict_fail_closed_without_values(self):
        with patch.dict(
            "os.environ",
            {"ROOKHOLD_API_KEY": "new-secret", "COOP_API_KEY": "old-secret"},
            clear=True,
        ):
            with self.assertRaisesRegex(
                ValueError, "ROOKHOLD_API_KEY conflicts"
            ) as caught:
                _compatible_env("ROOKHOLD_API_KEY", "COOP_API_KEY")
        self.assertNotIn("new-secret", str(caught.exception))
        self.assertNotIn("old-secret", str(caught.exception))

    def test_main_catches_environment_conflict_without_traceback_or_values(self):
        errors = io.StringIO()
        with (
            patch("sys.stderr", errors),
            patch.dict(
                "os.environ",
                {"ROOKHOLD_API_KEY": "new-secret", "COOP_API_KEY": "old-secret"},
                clear=True,
            ),
        ):
            self.assertEqual(main([]), 1)
        rendered = errors.getvalue()
        self.assertIn("ROOKHOLD_API_KEY conflicts", rendered)
        self.assertNotIn("new-secret", rendered)
        self.assertNotIn("old-secret", rendered)
        self.assertNotIn("Traceback", rendered)

    def test_json_run_emits_one_machine_readable_document(self):
        output = io.StringIO()
        fake = FakeRookhold()
        json_config = CliConfig(
            base_url="http://127.0.0.1:7300",
            api_key="secret-never-print",
            minimum_isolation="none",
            wait_seconds=60,
            color=False,
            json_output=True,
        )
        cli = RookholdCli(json_config, client=fake, output_stream=output)  # type: ignore[arg-type]
        cli.run_code("python", "print(42)")
        document = json.loads(output.getvalue())
        self.assertEqual(document["stdout"], "42")
        self.assertNotIn("accepted", output.getvalue())


if __name__ == "__main__":
    unittest.main()
