"""Dependency-free interactive terminal client for Rookhold.

``rookhold-cli`` is a human/operator surface. It talks to the same authenticated
HTTP API as the SDK and can inspect the exact MCP tool surface without exposing
the bearer key to model-visible arguments.
"""

from __future__ import annotations

import argparse
import json
import os
import shlex
import sys
from dataclasses import dataclass
from typing import Any, Dict, List, Mapping, Optional, Sequence, TextIO, cast

from rookhold import IsolationClass, Rookhold, RookholdError, __version__
from rookhold_mcp import McpConfig, RookholdMcpServer

DEFAULT_BASE_URL = "http://127.0.0.1:7300"
LANGUAGES = ("python", "node", "bash")
ISOLATION_CLASSES = (
    "none",
    "linux-shared-kernel",
    "gvisor-application-kernel",
    "wasm-capability",
    "hardware-vm",
    "confidential-vm",
)


def _compatible_env(primary: str, legacy: str, default: str = "") -> str:
    current = os.environ.get(primary)
    old = os.environ.get(legacy)
    if current not in {None, ""} and old not in {None, ""} and current != old:
        raise ValueError(
            f"{primary} conflicts with legacy compatibility variable {legacy}"
        )
    if isinstance(current, str) and current:
        return current
    if isinstance(old, str) and old:
        return old
    return default


def _as_list(value: Any) -> List[Any]:
    if isinstance(value, list):
        return cast(List[Any], value)  # type: ignore[redundant-cast]
    return []


@dataclass(frozen=True)
class CliConfig:
    base_url: str
    api_key: str
    minimum_isolation: IsolationClass
    wait_seconds: int
    color: bool
    json_output: bool


class Palette:
    """High-contrast terminal roles for the black, white, and electric-blue CLI."""

    def __init__(self, enabled: bool) -> None:
        self.enabled = enabled

    def paint(self, code: str, text: object) -> str:
        value = str(text)
        return f"\x1b[{code}m{value}\x1b[0m" if self.enabled else value

    def accent(self, text: object) -> str:
        return self.paint("38;2;121;160;255", text)

    def success(self, text: object) -> str:
        return self.paint("38;2;93;211;158", text)

    def warning(self, text: object) -> str:
        return self.paint("38;2;244;189;106", text)

    def danger(self, text: object) -> str:
        return self.paint("38;2;255;107;122", text)

    def muted(self, text: object) -> str:
        return self.paint("38;2;142;150;166", text)

    def strong(self, text: object) -> str:
        return self.paint("1;38;2;245;245;245", text)


class RookholdCli:
    """Interactive and one-shot operator client with injectable I/O for tests."""

    def __init__(
        self,
        config: CliConfig,
        *,
        client: Optional[Rookhold] = None,
        input_stream: TextIO = sys.stdin,
        output_stream: TextIO = sys.stdout,
        error_stream: TextIO = sys.stderr,
    ) -> None:
        self.config = config
        self.client = client or Rookhold(config.base_url, config.api_key, timeout=30)
        self.input = input_stream
        self.output = output_stream
        self.error = error_stream
        self.colors = Palette(config.color)
        self.last_job_id: Optional[str] = None
        self.identity: Optional[Mapping[str, Any]] = None
        self.server_capabilities: Optional[Mapping[str, Any]] = None

    def _write(self, value: str = "") -> None:
        self.output.write(value + "\n")
        self.output.flush()

    def _redact(self, value: object) -> str:
        text = str(value)
        return (
            text.replace(self.config.api_key, "[redacted]")
            if self.config.api_key
            else text
        )

    def connect(self) -> None:
        self.identity = cast(Mapping[str, Any], self.client.whoami())
        self.server_capabilities = cast(Mapping[str, Any], self.client.capabilities())

    def _execution(self) -> Mapping[str, Any]:
        capabilities = self.server_capabilities or {}
        value = capabilities.get("execution")
        return cast(Mapping[str, Any], value) if isinstance(value, dict) else {}

    def _mcp_tool_names(self) -> List[str]:
        capabilities = self.server_capabilities or {}
        raw_languages = capabilities.get("languages", [])
        language_values = _as_list(raw_languages)
        languages = [
            value
            for value in language_values
            if isinstance(value, str) and value in LANGUAGES
        ]
        if not languages:
            languages = list(LANGUAGES)
        server = RookholdMcpServer(
            McpConfig(
                base_url=self.config.base_url,
                api_key=self.config.api_key,
                allowed_languages=languages,
                max_wait_seconds=self.config.wait_seconds,
                max_code_bytes=512 * 1024,
                require_isolation=False,
                minimum_isolation=self.config.minimum_isolation,
            ),
            client=self.client,
        )
        initialized = server.handle(
            {
                "jsonrpc": "2.0",
                "id": "rookhold-cli-init",
                "method": "initialize",
                "params": {"protocolVersion": "2025-11-25"},
            }
        )
        if initialized is None or "error" in initialized:
            raise RuntimeError("MCP initialization failed")
        server.handle({"jsonrpc": "2.0", "method": "notifications/initialized"})
        response = server.handle(
            {"jsonrpc": "2.0", "id": "rookhold-cli-tools", "method": "tools/list"}
        )
        if response is None or "error" in response:
            raise RuntimeError("MCP tool discovery failed")
        result = response.get("result")
        tools = result.get("tools") if isinstance(result, dict) else None
        if not isinstance(tools, list):
            raise RuntimeError("MCP tool discovery returned an invalid response")
        return [
            cast(str, tool["name"])
            for tool in tools
            if isinstance(tool, dict) and isinstance(tool.get("name"), str)
        ]

    def banner(self) -> None:
        identity = self.identity or {}
        execution = self._execution()
        tenant = identity.get("tenant", "unknown")
        backend = execution.get("backend", "unknown")
        isolation = execution.get("isolation_class", "unknown")
        tools = self._mcp_tool_names()
        logo = (
            "    ██  ██\n"
            "  ██████████\n"
            "  ██████████\n"
            "    ██████\n"
            "    ██████\n"
            "  ██████████"
        )
        self._write(self.colors.accent(logo))
        self._write(self.colors.strong(f"Rookhold CLI v{__version__}"))
        self._write(self.colors.muted("Controlled execution for AI agents"))
        self._write()
        self._write(f"  Endpoint   {self.config.base_url}")
        self._write(f"  Tenant     {tenant}")
        self._write(f"  Runtime    {backend}")
        self._write(f"  Isolation  {isolation}")
        self._write(f"  MCP        {len(tools)} tools ready")
        self._write()
        if isolation == "none":
            self._write(
                self.colors.warning(
                    "  local demo · unisolated · submit trusted code only"
                )
            )
            self._write()
        self._write(self.colors.muted("Type /help for commands · /quit to exit"))

    def _job_id(self, candidate: Optional[str]) -> str:
        value = candidate or self.last_job_id
        if not value:
            raise ValueError("no job selected; provide a job ID or run a job first")
        return value

    def _print_json(self, value: object) -> None:
        self._write(json.dumps(value, indent=2, sort_keys=True, ensure_ascii=False))

    def _print_result(self, result: Mapping[str, Any]) -> None:
        status = result.get("status", "unknown")
        if status == "succeeded":
            status_text = self.colors.success(f"✓ {status}")
        elif status == "failed":
            status_text = self.colors.danger(f"× {status}")
        elif status == "cancelled":
            status_text = self.colors.muted(f"■ {status}")
        else:
            status_text = self.colors.warning(str(status))
        self._write(
            f"  {status_text} · exit {result.get('exit_code', '—')} · "
            f"{result.get('duration_ms', '—')} ms"
        )
        stdout = result.get("stdout")
        stderr = result.get("stderr")
        if stdout:
            self._write(self.colors.accent("  stdout"))
            for line in str(stdout).splitlines():
                self._write(f"    {line}")
        if stderr:
            self._write(self.colors.warning("  stderr"))
            for line in str(stderr).splitlines():
                self._write(f"    {line}")
        violations = result.get("violations")
        if isinstance(violations, list) and violations:
            violation_values = _as_list(violations)
            self._write(self.colors.warning(f"  violations {len(violation_values)}"))

    def run_code(self, language: str, code: str) -> Mapping[str, Any]:
        if language not in LANGUAGES:
            raise ValueError("language must be python, node, or bash")
        if not code.strip():
            raise ValueError("code must not be empty")
        submitted = self.client.submit_result(
            language,
            code,
            requirements={"minimum_isolation": self.config.minimum_isolation},
        )
        job = submitted["job"]
        job_id = job["job_id"]
        self.last_job_id = job_id
        if not self.config.json_output:
            self._write(
                f"  {self.colors.accent('accepted')} · job {job_id} · waiting up to "
                f"{self.config.wait_seconds}s"
            )
        result = cast(
            Mapping[str, Any],
            self.client.result(job_id, timeout=self.config.wait_seconds),
        )
        if self.config.json_output:
            self._print_json(result)
        else:
            self._print_result(result)
        return result

    def posture(self) -> Mapping[str, Any]:
        if self.server_capabilities is None:
            self.connect()
        execution = self._execution()
        raw_limits: Any = (
            self.server_capabilities.get("limits", {})
            if self.server_capabilities
            else {}
        )
        limits: Mapping[str, Any] = (
            cast(Mapping[str, Any], raw_limits) if isinstance(raw_limits, dict) else {}
        )
        value: Dict[str, Any] = {
            "base_url": self.config.base_url,
            "tenant": (self.identity or {}).get("tenant"),
            "backend": execution.get("backend"),
            "isolation_class": execution.get("isolation_class"),
            "networking": execution.get("networking"),
            "minimum_isolation": self.config.minimum_isolation,
            "languages": (self.server_capabilities or {}).get("languages", []),
            "limits": limits,
        }
        if self.config.json_output:
            self._print_json(value)
        else:
            self._write(f"  Runtime    {value['backend']}")
            self._write(f"  Isolation  {value['isolation_class']}")
            self._write(f"  Required   {value['minimum_isolation']}")
            self._write(f"  Network    {value['networking']}")
            self._write(f"  Languages  {', '.join(value['languages'])}")
        return value

    def mcp_tools(self) -> List[str]:
        if self.server_capabilities is None:
            self.connect()
        names = self._mcp_tool_names()
        if self.config.json_output:
            self._print_json({"server": "rookhold-mcp", "tools": names})
        else:
            self._write(self.colors.accent("  rookhold-mcp"))
            for name in names:
                self._write(f"    {self.colors.success('●')} {name}")
            self._write(
                self.colors.muted(f"  policy minimum: {self.config.minimum_isolation}")
            )
        return names

    def jobs(self, limit: int = 10) -> Mapping[str, Any]:
        page = cast(Mapping[str, Any], self.client.list(limit=limit))
        if self.config.json_output:
            self._print_json(page)
            return page
        jobs = page.get("items", page.get("jobs", []))
        if not isinstance(jobs, list) or not jobs:
            self._write(self.colors.muted("  no jobs"))
            return page
        for raw_job in _as_list(jobs):
            job = (
                cast(Mapping[str, Any], raw_job) if isinstance(raw_job, dict) else None
            )
            if not isinstance(job, dict):
                continue
            status = job.get("status", "unknown")
            if status == "succeeded":
                marker = self.colors.success("●")
            elif status == "failed":
                marker = self.colors.danger("●")
            elif status == "cancelled":
                marker = self.colors.muted("■")
            else:
                marker = self.colors.warning("●")
            self._write(
                f"  {marker} {str(job.get('job_id', ''))[:18]:18} "
                f"{str(status):12} {job.get('language', 'unknown')}"
            )
        return page

    def detail(self, job_id: Optional[str]) -> Mapping[str, Any]:
        selected = self._job_id(job_id)
        value = cast(Mapping[str, Any], self.client.get(selected))
        self.last_job_id = selected
        self._print_json(value)
        return value

    def result(
        self, job_id: Optional[str], wait_seconds: Optional[int] = None
    ) -> Mapping[str, Any]:
        selected = self._job_id(job_id)
        value = cast(
            Mapping[str, Any],
            self.client.result(
                selected,
                timeout=self.config.wait_seconds
                if wait_seconds is None
                else wait_seconds,
            ),
        )
        self.last_job_id = selected
        if self.config.json_output:
            self._print_json(value)
        else:
            self._print_result(value)
        return value

    def events(
        self, job_id: Optional[str], after: int = 0
    ) -> Sequence[Mapping[str, Any]]:
        selected = self._job_id(job_id)
        values = cast(
            Sequence[Mapping[str, Any]], self.client.replay(selected, after=after)
        )
        self.last_job_id = selected
        if self.config.json_output:
            self._print_json(values)
        else:
            for event in values:
                self._write(
                    f"  {str(event.get('seq', '—')):>4} "
                    f"{str(event.get('kind', 'event')):10} {event.get('data', '')}"
                )
        return values

    def cancel(self, job_id: Optional[str]) -> Mapping[str, Any]:
        selected = self._job_id(job_id)
        value = cast(Mapping[str, Any], self.client.cancel_result(selected))
        self.last_job_id = selected
        if self.config.json_output:
            self._print_json(value)
        else:
            state = (
                "requested"
                if value.get("cancellation_requested")
                else "already terminal"
            )
            self._write(f"  cancellation {state} · job {selected}")
        return value

    def help(self) -> None:
        self._write("  /run LANGUAGE CODE       submit and wait for a short job")
        self._write("  /paste LANGUAGE          enter multiline code; finish with .end")
        self._write("  /jobs [LIMIT]            list recent tenant jobs")
        self._write("  /show [JOB_ID]           inspect the complete job record")
        self._write("  /result [JOB_ID]         wait for and show a result")
        self._write("  /events [JOB_ID]         replay persisted events")
        self._write("  /cancel [JOB_ID]         cancel a queued or running job")
        self._write("  /posture                 show observed server posture")
        self._write("  /mcp                     inspect the live MCP tool surface")
        self._write("  /clear                   clear the terminal")
        self._write("  /quit                    exit")

    def _paste(self, language: str) -> None:
        if language not in LANGUAGES:
            raise ValueError("language must be python, node, or bash")
        self._write(self.colors.muted("  paste code; enter .end on its own line"))
        lines: List[str] = []
        while True:
            raw = self.input.readline()
            if raw == "":
                raise EOFError
            if raw.rstrip("\r\n") == ".end":
                break
            lines.append(raw.rstrip("\r\n"))
        self.run_code(language, "\n".join(lines))

    def dispatch(self, line: str) -> bool:
        stripped = line.strip()
        if not stripped:
            return True
        if stripped.startswith("/"):
            stripped = stripped[1:]
        parts = shlex.split(stripped)
        if not parts:
            return True
        command, args = parts[0].lower(), parts[1:]
        if command in {"quit", "exit", "q"}:
            return False
        if command in {"help", "?"}:
            self.help()
        elif command == "run":
            if len(args) < 2:
                raise ValueError("usage: /run LANGUAGE CODE")
            self.run_code(args[0].lower(), " ".join(args[1:]))
        elif command == "paste":
            if len(args) != 1:
                raise ValueError("usage: /paste LANGUAGE")
            self._paste(args[0].lower())
        elif command == "jobs":
            self.jobs(int(args[0]) if args else 10)
        elif command == "show":
            self.detail(args[0] if args else None)
        elif command == "result":
            self.result(args[0] if args else None)
        elif command == "events":
            self.events(args[0] if args else None)
        elif command == "cancel":
            self.cancel(args[0] if args else None)
        elif command in {"posture", "status"}:
            self.posture()
        elif command == "mcp":
            self.mcp_tools()
        elif command == "clear":
            self._write("\x1b[2J\x1b[H" if self.config.color else "")
        else:
            raise ValueError(f"unknown command: {command}; type /help")
        return True

    def shell(self) -> int:
        self.connect()
        self.banner()
        while True:
            self.output.write(self.colors.accent("rookhold › "))
            self.output.flush()
            line = self.input.readline()
            if line == "":
                self._write()
                return 0
            try:
                if not self.dispatch(line):
                    return 0
            except EOFError:
                self._write()
                return 0
            except (
                RookholdError,
                RuntimeError,
                TimeoutError,
                TypeError,
                ValueError,
            ) as exc:
                self._write(self.colors.danger(f"  error · {self._redact(exc)}"))


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="rookhold-cli",
        description="Interactive terminal client for the Rookhold execution service",
    )
    parser.add_argument(
        "--base-url",
        default=_compatible_env("ROOKHOLD_BASE_URL", "COOP_BASE_URL", DEFAULT_BASE_URL),
    )
    parser.add_argument(
        "--api-key",
        default=_compatible_env("ROOKHOLD_API_KEY", "COOP_API_KEY"),
        help="tenant API key; prefer ROOKHOLD_API_KEY to avoid process-list exposure",
    )
    parser.add_argument(
        "--minimum-isolation",
        choices=ISOLATION_CLASSES,
        default=_compatible_env(
            "ROOKHOLD_CLI_MINIMUM_ISOLATION",
            "COOP_CLI_MINIMUM_ISOLATION",
            "none",
        ),
    )
    parser.add_argument("--wait-seconds", type=int, default=60)
    parser.add_argument("--json", action="store_true", dest="json_output")
    parser.add_argument("--no-color", action="store_true")
    commands = parser.add_subparsers(dest="command")
    commands.add_parser("shell", help="open the interactive terminal (default)")
    run = commands.add_parser("run", help="submit code and wait for its result")
    run.add_argument("language", choices=LANGUAGES)
    run.add_argument("code", nargs="+")
    jobs = commands.add_parser("jobs", help="list recent tenant jobs")
    jobs.add_argument("--limit", type=int, default=10)
    show = commands.add_parser("show", help="inspect one job")
    show.add_argument("job_id")
    result = commands.add_parser("result", help="wait for one job result")
    result.add_argument("job_id")
    result.add_argument("--wait", type=int)
    events = commands.add_parser("events", help="replay persisted job events")
    events.add_argument("job_id")
    events.add_argument("--after", type=int, default=0)
    cancel = commands.add_parser("cancel", help="cancel one job")
    cancel.add_argument("job_id")
    commands.add_parser("posture", help="show observed server posture")
    commands.add_parser("mcp", help="inspect the live rookhold-mcp tools")
    return parser


def _config(args: argparse.Namespace, output: TextIO) -> CliConfig:
    if not args.api_key.strip():
        raise ValueError("ROOKHOLD_API_KEY is required")
    if not 1 <= args.wait_seconds <= 300:
        raise ValueError("--wait-seconds must be between 1 and 300")
    color = not args.no_color and "NO_COLOR" not in os.environ and output.isatty()
    return CliConfig(
        base_url=args.base_url,
        api_key=args.api_key,
        minimum_isolation=cast(IsolationClass, args.minimum_isolation),
        wait_seconds=args.wait_seconds,
        color=color,
        json_output=args.json_output,
    )


def main(argv: Optional[Sequence[str]] = None) -> int:
    try:
        parser = _parser()
        args = parser.parse_args(argv)
        cli = RookholdCli(_config(args, sys.stdout))
        command = args.command or "shell"
        if command == "shell":
            return cli.shell()
        cli.connect()
        if command == "run":
            cli.run_code(args.language, " ".join(args.code))
        elif command == "jobs":
            cli.jobs(args.limit)
        elif command == "show":
            cli.detail(args.job_id)
        elif command == "result":
            cli.result(args.job_id, args.wait)
        elif command == "events":
            cli.events(args.job_id, args.after)
        elif command == "cancel":
            cli.cancel(args.job_id)
        elif command == "posture":
            cli.posture()
        elif command == "mcp":
            cli.mcp_tools()
        return 0
    except (RookholdError, RuntimeError, TimeoutError, TypeError, ValueError) as exc:
        key = getattr(locals().get("args"), "api_key", "")
        message = str(exc).replace(key, "[redacted]") if key else str(exc)
        print(f"rookhold-cli: {message}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
