"""Dependency-free stdio MCP adapter for the Coop execution gateway.

The adapter deliberately keeps the Coop URL and bearer key outside model-visible
tool arguments. It implements the stable newline-delimited stdio transport so
the same installed ``coop-mcp`` command works with Hermes, OpenClaw, and other
MCP hosts without running a second network service.
"""

from __future__ import annotations

import json
import math
import os
import sys
from typing import Any, Dict, List, Mapping, Optional, Sequence, TextIO, Union, cast

from coop import Capabilities, Coop, CoopError

__version__ = "0.3.0"

PROTOCOL_VERSION = "2025-11-25"
SUPPORTED_PROTOCOL_VERSIONS = frozenset(
    {PROTOCOL_VERSION, "2025-06-18", "2025-03-26", "2024-11-05"}
)
DEFAULT_BASE_URL = "http://127.0.0.1:7300"
DEFAULT_MAX_WAIT_SECONDS = 300
DEFAULT_MAX_CODE_BYTES = 512 * 1024
LANGUAGES = frozenset({"python", "node", "bash"})

JsonScalar = Union[None, bool, int, float, str]
JsonValue = Union[JsonScalar, List["JsonValue"], Dict[str, "JsonValue"]]
JsonObject = Dict[str, JsonValue]


def _json_object(value: Mapping[str, Any]) -> JsonObject:
    """Bridge SDK TypedDict records into the recursive JSON value type."""
    return cast(JsonObject, dict(value))


def _object_schema(
    properties: Mapping[str, JsonValue], required: Sequence[str] = ()
) -> JsonObject:
    schema: JsonObject = {
        "type": "object",
        "properties": dict(properties),
        "additionalProperties": False,
    }
    if required:
        schema["required"] = list(required)
    return schema


LIMITS_SCHEMA = _object_schema(
    {
        "wall_seconds": {"type": "integer", "minimum": 1, "maximum": 300},
        "cpu_seconds": {"type": "integer", "minimum": 1, "maximum": 300},
        "mem_mb": {"type": "integer", "minimum": 16, "maximum": 4096},
        "max_pids": {"type": "integer", "minimum": 1, "maximum": 1024},
        "max_file_mb": {"type": "integer", "minimum": 1, "maximum": 1024},
        "allow_network": {"type": "boolean"},
    }
)

TOOLS: List[JsonObject] = [
    {
        "name": "coop_run_code",
        "title": "Run code with Coop",
        "description": (
            "Run one short Python, Node.js, or Bash job under Coop policy and return "
            "its bounded stdout, stderr, terminal status, violations, job ID, and "
            "evidence receipt. Use this instead of a local shell for generated or "
            "untrusted snippets. It is stateless: files and installed packages do not "
            "carry into the next call."
        ),
        "inputSchema": _object_schema(
            {
                "language": _json_object({"type": "string", "enum": sorted(LANGUAGES)}),
                "code": {
                    "type": "string",
                    "description": "Complete source code for one short-lived job.",
                },
                "stdin": {"type": "string"},
                "limits": LIMITS_SCHEMA,
                "wait_seconds": {
                    "type": "number",
                    "minimum": 1,
                    "maximum": DEFAULT_MAX_WAIT_SECONDS,
                    "default": 60,
                    "description": (
                        "How long the adapter waits for a terminal result. The job keeps "
                        "running if this wait expires; use coop_job_result afterward."
                    ),
                },
            },
            ("language", "code"),
        ),
        "annotations": {
            "readOnlyHint": False,
            "destructiveHint": False,
            "idempotentHint": False,
            "openWorldHint": False,
        },
    },
    {
        "name": "coop_job_result",
        "title": "Get a Coop job result",
        "description": (
            "Wait for a previously submitted Coop job and return its bounded terminal "
            "result. Use wait_seconds=0 for an immediate status snapshot."
        ),
        "inputSchema": _object_schema(
            {
                "job_id": {"type": "string", "minLength": 1},
                "wait_seconds": {
                    "type": "number",
                    "minimum": 0,
                    "maximum": DEFAULT_MAX_WAIT_SECONDS,
                    "default": 60,
                },
            },
            ("job_id",),
        ),
        "annotations": {
            "readOnlyHint": True,
            "destructiveHint": False,
            "idempotentHint": True,
            "openWorldHint": False,
        },
    },
    {
        "name": "coop_job_events",
        "title": "Inspect Coop job evidence",
        "description": (
            "Read a cursor-bounded page of persisted lifecycle, output, violation, and "
            "terminal evidence events for a Coop job."
        ),
        "inputSchema": _object_schema(
            {
                "job_id": {"type": "string", "minLength": 1},
                "after": {"type": "integer", "minimum": -1},
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 500,
                    "default": 100,
                },
            },
            ("job_id",),
        ),
        "annotations": {
            "readOnlyHint": True,
            "destructiveHint": False,
            "idempotentHint": True,
            "openWorldHint": False,
        },
    },
    {
        "name": "coop_cancel_job",
        "title": "Cancel a Coop job",
        "description": "Cancel one queued or running Coop job owned by this tenant.",
        "inputSchema": _object_schema(
            {"job_id": {"type": "string", "minLength": 1}}, ("job_id",)
        ),
        "annotations": {
            "readOnlyHint": False,
            "destructiveHint": True,
            "idempotentHint": False,
            "openWorldHint": False,
        },
    },
]


class McpConfig:
    """Operator-owned adapter policy loaded from the process environment."""

    def __init__(
        self,
        *,
        base_url: str,
        api_key: str,
        allowed_languages: Sequence[str],
        max_wait_seconds: int,
        max_code_bytes: int,
        require_isolation: bool,
    ) -> None:
        if not api_key.strip():
            raise ValueError("COOP_API_KEY is required")
        allowed = frozenset(allowed_languages)
        if not allowed or not allowed.issubset(LANGUAGES):
            raise ValueError(
                "COOP_MCP_ALLOWED_LANGUAGES must be a non-empty comma-separated "
                "subset of python,node,bash"
            )
        if not 1 <= max_wait_seconds <= 300:
            raise ValueError("COOP_MCP_MAX_WAIT_SECONDS must be between 1 and 300")
        if not 1 <= max_code_bytes <= 1024 * 1024:
            raise ValueError("COOP_MCP_MAX_CODE_BYTES must be between 1 and 1048576")
        self.base_url = base_url
        self.api_key = api_key
        self.allowed_languages = allowed
        self.max_wait_seconds = max_wait_seconds
        self.max_code_bytes = max_code_bytes
        self.require_isolation = require_isolation

    @classmethod
    def from_env(cls) -> "McpConfig":
        languages = [
            value.strip().lower()
            for value in os.environ.get(
                "COOP_MCP_ALLOWED_LANGUAGES", "python,node,bash"
            ).split(",")
            if value.strip()
        ]
        return cls(
            base_url=os.environ.get("COOP_BASE_URL", DEFAULT_BASE_URL),
            api_key=os.environ.get("COOP_API_KEY", ""),
            allowed_languages=languages,
            max_wait_seconds=_env_int(
                "COOP_MCP_MAX_WAIT_SECONDS", DEFAULT_MAX_WAIT_SECONDS
            ),
            max_code_bytes=_env_int("COOP_MCP_MAX_CODE_BYTES", DEFAULT_MAX_CODE_BYTES),
            require_isolation=_env_bool("COOP_MCP_REQUIRE_ISOLATION", False),
        )


def _env_int(name: str, default: int) -> int:
    raw = os.environ.get(name)
    if raw is None:
        return default
    try:
        return int(raw)
    except ValueError as exc:
        raise ValueError(f"{name} must be an integer") from exc


def _env_bool(name: str, default: bool) -> bool:
    raw = os.environ.get(name)
    if raw is None:
        return default
    normalized = raw.strip().lower()
    if normalized in {"1", "true", "yes", "on"}:
        return True
    if normalized in {"0", "false", "no", "off"}:
        return False
    raise ValueError(f"{name} must be true or false")


class CoopMcpServer:
    """Small synchronous MCP server whose tool handlers call one Coop tenant."""

    def __init__(self, config: McpConfig, client: Optional[Coop] = None) -> None:
        self.config = config
        self.client = client or Coop(config.base_url, config.api_key)
        self.initialized = False
        self._capabilities: Optional[Capabilities] = None

    def handle(self, message: JsonObject) -> Optional[JsonObject]:
        request_id = message.get("id")
        is_notification = "id" not in message
        if message.get("jsonrpc") != "2.0" or not isinstance(
            message.get("method"), str
        ):
            if is_notification:
                return None
            return _rpc_error(request_id, -32600, "invalid JSON-RPC request")

        method = message["method"]
        params_value = message.get("params", {})
        if not isinstance(params_value, dict):
            if is_notification:
                return None
            return _rpc_error(request_id, -32602, "params must be an object")
        params = params_value

        if is_notification:
            if method == "notifications/initialized":
                self.initialized = True
            return None

        if method == "initialize":
            requested = params.get("protocolVersion")
            protocol = (
                requested
                if isinstance(requested, str)
                and requested in SUPPORTED_PROTOCOL_VERSIONS
                else PROTOCOL_VERSION
            )
            return _rpc_result(
                request_id,
                {
                    "protocolVersion": protocol,
                    "capabilities": {"tools": {"listChanged": False}},
                    "serverInfo": {"name": "coop-mcp", "version": __version__},
                    "instructions": (
                        "Use coop_run_code for short stateless generated snippets. "
                        "Do not use it for persistent workspaces or interactive shells. "
                        "The Coop URL, key, language allowlist, and isolation requirement "
                        "are operator-controlled and cannot be chosen by the model."
                    ),
                },
            )

        if not self.initialized:
            return _rpc_error(request_id, -32002, "server is not initialized")
        if method == "ping":
            return _rpc_result(request_id, {})
        if method == "tools/list":
            return _rpc_result(
                request_id, _json_object({"tools": self._listed_tools()})
            )
        if method == "tools/call":
            return _rpc_result(request_id, self._call_tool(params))
        return _rpc_error(request_id, -32601, f"method not found: {method}")

    def _listed_tools(self) -> List[JsonValue]:
        """Return schemas narrowed to this operator-owned adapter policy."""
        tools: Any = json.loads(json.dumps(TOOLS))
        tools[0]["inputSchema"]["properties"]["language"]["enum"] = sorted(
            self.config.allowed_languages
        )
        tools[0]["inputSchema"]["properties"]["wait_seconds"]["maximum"] = (
            self.config.max_wait_seconds
        )
        tools[1]["inputSchema"]["properties"]["wait_seconds"]["maximum"] = (
            self.config.max_wait_seconds
        )
        return cast(List[JsonValue], tools)

    def _call_tool(self, params: JsonObject) -> JsonObject:
        name = params.get("name")
        arguments_value = params.get("arguments", {})
        if not isinstance(name, str):
            return _tool_error("tool name must be a string")
        if not isinstance(arguments_value, dict):
            return _tool_error("tool arguments must be an object")
        arguments = arguments_value
        try:
            if name == "coop_run_code":
                return _tool_success(self._run_code(arguments))
            if name == "coop_job_result":
                return _tool_success(self._job_result(arguments))
            if name == "coop_job_events":
                return _tool_success(self._job_events(arguments))
            if name == "coop_cancel_job":
                return _tool_success(self._cancel_job(arguments))
            return _tool_error(f"unknown tool: {name}")
        except (CoopError, RuntimeError, TimeoutError, TypeError, ValueError) as exc:
            return _tool_error(self._redact(str(exc)))

    def _ensure_posture(self) -> Capabilities:
        if self._capabilities is None:
            self._capabilities = self.client.capabilities()
        capabilities = self._capabilities
        execution = capabilities["execution"]
        if self.config.require_isolation:
            enforcement = execution["limit_enforcement"]
            required = (
                execution["isolated"],
                execution["private_rootfs"],
                execution["dedicated_bootstrap"],
                execution["seccomp"],
                execution["networking"] == "disabled",
                all(enforcement.values()),
            )
            if not all(required):
                raise RuntimeError(
                    "Coop MCP policy requires the isolated namespace backend with a "
                    "private rootfs, dedicated bootstrap, seccomp, disabled networking, "
                    "and all resource controls enforced"
                )
        return capabilities

    def _run_code(self, arguments: JsonObject) -> JsonObject:
        _reject_unknown(
            arguments, {"language", "code", "stdin", "limits", "wait_seconds"}
        )
        language = _required_string(arguments, "language")
        code = _required_string(arguments, "code", allow_empty=True)
        if language not in self.config.allowed_languages:
            raise ValueError(f"language is not allowed by adapter policy: {language}")
        capabilities = self._ensure_posture()
        if language not in capabilities["languages"]:
            raise ValueError(f"runtime is unavailable on the Coop server: {language}")
        code_bytes = len(code.encode("utf-8"))
        max_code = min(
            self.config.max_code_bytes, capabilities["limits"]["code_bytes_max"]
        )
        if code_bytes > max_code:
            raise ValueError(
                f"code exceeds the adapter limit of {max_code} UTF-8 bytes"
            )
        stdin_value = arguments.get("stdin")
        if stdin_value is not None and not isinstance(stdin_value, str):
            raise TypeError("stdin must be a string")
        limits_value = arguments.get("limits")
        if limits_value is not None and not isinstance(limits_value, dict):
            raise TypeError("limits must be an object")
        limits = cast(Optional[Mapping[str, Any]], limits_value)
        wait_seconds = self._wait_seconds(arguments, default=60, allow_zero=False)

        submitted = self.client.submit(
            language,
            code,
            stdin=stdin_value,
            limits=limits,
        )
        job_id = submitted["job_id"]
        try:
            result = _json_object(self.client.result(job_id, timeout=wait_seconds))
            self._attach_evidence(result, job_id)
            result["complete"] = True
            return result
        except TimeoutError:
            snapshot = _json_object(self.client.get(job_id))
            return {
                "job_id": job_id,
                "status": snapshot.get("status"),
                "complete": False,
                "message": (
                    "The adapter wait expired; the job is still owned by Coop. Call "
                    "coop_job_result with this job_id or cancel it."
                ),
            }

    def _job_result(self, arguments: JsonObject) -> JsonObject:
        _reject_unknown(arguments, {"job_id", "wait_seconds"})
        job_id = _required_string(arguments, "job_id")
        wait_seconds = self._wait_seconds(arguments, default=60, allow_zero=True)
        if wait_seconds == 0:
            snapshot = _json_object(self.client.get(job_id))
            snapshot["complete"] = str(snapshot.get("status")) not in {
                "queued",
                "running",
            }
            return snapshot
        try:
            result = _json_object(self.client.result(job_id, timeout=wait_seconds))
            self._attach_evidence(result, job_id)
            result["complete"] = True
            return result
        except TimeoutError:
            snapshot = _json_object(self.client.get(job_id))
            snapshot["complete"] = False
            return snapshot

    def _job_events(self, arguments: JsonObject) -> JsonObject:
        _reject_unknown(arguments, {"job_id", "after", "limit"})
        job_id = _required_string(arguments, "job_id")
        after = _optional_int(arguments, "after", minimum=-1, default=None)
        limit = _optional_int(arguments, "limit", minimum=1, maximum=500, default=100)
        return _json_object(
            self.client.event_page(job_id, after=after, limit=cast(int, limit))
        )

    def _cancel_job(self, arguments: JsonObject) -> JsonObject:
        _reject_unknown(arguments, {"job_id"})
        job_id = _required_string(arguments, "job_id")
        cancelled = self.client.cancel(job_id)
        job = None if cancelled is None else _json_object(cancelled)
        return {"job_id": job_id, "cancelled": cancelled is not None, "job": job}

    def _attach_evidence(self, result: JsonObject, job_id: str) -> None:
        detail = _json_object(self.client.get(job_id))
        for field in (
            "effective_spec",
            "execution_policy",
            "receipt",
            "receipt_sha256",
        ):
            result[field] = detail.get(field)

    def _wait_seconds(
        self, arguments: JsonObject, *, default: int, allow_zero: bool
    ) -> float:
        raw = arguments.get("wait_seconds", default)
        if isinstance(raw, bool) or not isinstance(raw, (int, float)):
            raise TypeError("wait_seconds must be a number")
        value = float(raw)
        minimum = 0 if allow_zero else 1
        if (
            not math.isfinite(value)
            or value < minimum
            or value > self.config.max_wait_seconds
        ):
            raise ValueError(
                f"wait_seconds must be between {minimum} and "
                f"{self.config.max_wait_seconds}"
            )
        return value

    def _redact(self, message: str) -> str:
        return message.replace(self.config.api_key, "[REDACTED]")


def _required_string(
    arguments: JsonObject, name: str, *, allow_empty: bool = False
) -> str:
    value = arguments.get(name)
    if not isinstance(value, str) or (not allow_empty and not value):
        suffix = "a string" if allow_empty else "a non-empty string"
        raise TypeError(f"{name} must be {suffix}")
    return value


def _optional_int(
    arguments: JsonObject,
    name: str,
    *,
    minimum: int,
    default: Optional[int],
    maximum: Optional[int] = None,
) -> Optional[int]:
    value = arguments.get(name, default)
    if value is None:
        return None
    if isinstance(value, bool) or not isinstance(value, int):
        raise TypeError(f"{name} must be an integer")
    if value < minimum or (maximum is not None and value > maximum):
        bound = f"at least {minimum}"
        if maximum is not None:
            bound = f"between {minimum} and {maximum}"
        raise ValueError(f"{name} must be {bound}")
    return value


def _reject_unknown(arguments: JsonObject, allowed: set[str]) -> None:
    unknown = set(arguments).difference(allowed)
    if unknown:
        raise TypeError(f"unknown argument(s): {', '.join(sorted(unknown))}")


def _rpc_result(request_id: JsonValue, result: JsonObject) -> JsonObject:
    return {"jsonrpc": "2.0", "id": request_id, "result": result}


def _rpc_error(
    request_id: JsonValue, code: int, message: str, data: Optional[JsonValue] = None
) -> JsonObject:
    error: JsonObject = {"code": code, "message": message}
    if data is not None:
        error["data"] = data
    return {"jsonrpc": "2.0", "id": request_id, "error": error}


def _tool_success(value: JsonObject) -> JsonObject:
    serialized = json.dumps(value, separators=(",", ":"), ensure_ascii=False)
    return {
        "content": [{"type": "text", "text": serialized}],
        "structuredContent": value,
        "isError": False,
    }


def _tool_error(message: str) -> JsonObject:
    return {"content": [{"type": "text", "text": message}], "isError": True}


def serve(
    server: CoopMcpServer,
    input_stream: TextIO = sys.stdin,
    output_stream: TextIO = sys.stdout,
) -> None:
    """Serve newline-delimited MCP JSON-RPC until the client closes stdin."""
    for line in input_stream:
        response: Optional[JsonObject]
        try:
            decoded = json.loads(line)
            if not isinstance(decoded, dict):
                response = _rpc_error(None, -32600, "request must be a JSON object")
            else:
                response = server.handle(cast(JsonObject, decoded))
        except (json.JSONDecodeError, UnicodeError) as exc:
            response = _rpc_error(None, -32700, f"parse error: {exc}")
        if response is not None:
            output_stream.write(
                json.dumps(response, separators=(",", ":"), ensure_ascii=False) + "\n"
            )
            output_stream.flush()


def main() -> None:
    try:
        config = McpConfig.from_env()
        serve(CoopMcpServer(config))
    except (OSError, ValueError) as exc:
        print(f"coop-mcp: {exc}", file=sys.stderr)
        raise SystemExit(2) from exc


if __name__ == "__main__":
    main()
