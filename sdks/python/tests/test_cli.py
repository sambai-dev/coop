import io
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from rookhold_cli import (
    CliConfig,
    Palette,
    RookholdCli,
    _compatible_env,
    _setup_host,
    main,
)


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
            "version": "0.8.0",
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
    def test_check_uses_plain_pass_warn_language_and_tests_mcp(self):
        output = io.StringIO()
        cli = RookholdCli(
            config(),
            client=FakeRookhold(),
            output_stream=output,  # type: ignore[arg-type]
        )
        value = cli.check()
        self.assertEqual(value["isolation"], "none")
        rendered = output.getvalue()
        self.assertIn("OK    service reachable", rendered)
        self.assertIn("WARN    isolation none", rendered)
        self.assertIn("MCP connection succeeded; 4 tools exposed", rendered)

    def test_setup_previews_writes_and_backs_up_without_copying_secrets(self):
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / ".mcp.json"
            target.write_text('{"mcpServers":{"other":{"command":"other"}}}\n')
            output = io.StringIO()
            configured = _setup_host(
                "claude-code",
                target,
                yes=True,
                input_stream=io.StringIO(),
                output_stream=output,
            )
            self.assertEqual(configured, target.resolve())
            value = json.loads(target.read_text())
            self.assertEqual(value["mcpServers"]["other"]["command"], "other")
            self.assertEqual(
                value["mcpServers"]["rookhold"],
                {"command": "rookhold-cli", "args": ["mcp-server"]},
            )
            self.assertNotIn("secret-never-print", target.read_text())
            self.assertEqual(len(list(target.parent.glob(".mcp.json.*.bak"))), 1)
            self.assertIn('+    "rookhold"', output.getvalue())

    def test_terminal_palette_uses_accessible_electric_blue_and_semantic_roles(self):
        colors = Palette(True)
        self.assertEqual(colors.accent("prompt"), "\x1b[38;2;121;160;255mprompt\x1b[0m")
        self.assertEqual(colors.strong("title"), "\x1b[1;38;2;245;245;245mtitle\x1b[0m")
        self.assertEqual(colors.danger("error"), "\x1b[38;2;255;107;122merror\x1b[0m")
        self.assertEqual(Palette(False).accent("prompt"), "prompt")

    def test_version_does_not_require_connection_configuration(self):
        output = io.StringIO()
        with self.assertRaises(SystemExit) as caught, patch("sys.stdout", output):
            main(["--version"])
        self.assertEqual(caught.exception.code, 0)
        self.assertEqual(output.getvalue(), "rookhold-cli 0.8.0\n")

    def test_same_cli_executable_can_launch_the_mcp_server(self):
        with patch("rookhold_cli.mcp_main") as mcp:
            self.assertEqual(main(["mcp-server"]), 0)
        mcp.assert_called_once_with([])

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
