"""Dependency-free stdio MCP adapter for the Coop execution gateway.

The adapter deliberately keeps the Coop URL and bearer key outside model-visible
tool arguments. It implements the stable newline-delimited stdio transport so
the same installed ``coop-mcp`` command works with Hermes, OpenClaw, and other
MCP hosts without running a second network service.
"""

from __future__ import annotations

import concurrent.futures
import json
import math
import os
import sys
import threading
import time
from datetime import datetime, timezone
from typing import (
    Any,
    Dict,
    List,
    Mapping,
    Optional,
    Sequence,
    TextIO,
    Union,
    cast,
)

from coop import Capabilities, Coop, CoopError

__version__ = "0.3.0"

PROTOCOL_VERSION = "2025-11-25"
MODERN_PROTOCOL_VERSION = "2026-07-28"
SUPPORTED_PROTOCOL_VERSIONS = frozenset(
    {PROTOCOL_VERSION, "2025-06-18", "2025-03-26", "2024-11-05"}
)
TASKS_EXTENSION = "io.modelcontextprotocol/tasks"
DEFAULT_BASE_URL = "http://127.0.0.1:7300"
DEFAULT_MAX_WAIT_SECONDS = 300
DEFAULT_MAX_CODE_BYTES = 512 * 1024
DEFAULT_TASK_POLL_INTERVAL_MS = 1_000
DEFAULT_TASK_TTL_MS = 3_600_000
MAX_CONCURRENT_REQUESTS = 16
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
        "max_pids": {"type": "integer", "minimum": 8, "maximum": 1024},
        "max_file_mb": {"type": "integer", "minimum": 1, "maximum": 1024},
        "allow_network": {"type": "boolean", "const": False},
    }
)

RUN_OUTPUT_SCHEMA: JsonObject = {
    "type": "object",
    "properties": {
        "job_id": {"type": "string"},
        "status": {"type": ["string", "null"]},
        "complete": {"type": "boolean"},
        "exit_code": {"type": ["integer", "null"]},
        "duration_ms": {"type": ["integer", "null"]},
        "stdout": {"type": "string"},
        "stderr": {"type": "string"},
        "truncated": {"type": "boolean"},
        "violations": {"type": "array"},
        "message": {"type": "string"},
        "error_code": {"type": "string"},
        "retryable": {"type": "boolean"},
        "effective_spec": {},
        "execution_policy": {},
        "receipt": {},
        "receipt_sha256": {"type": ["string", "null"]},
    },
    "required": ["job_id", "status", "complete"],
    "additionalProperties": True,
}

EVENTS_OUTPUT_SCHEMA: JsonObject = {
    "type": "object",
    "properties": {
        "events": {"type": "array", "items": {"type": "object"}},
        "next_cursor": {"type": ["integer", "null"]},
    },
    "required": ["events", "next_cursor"],
    "additionalProperties": False,
}

CANCEL_OUTPUT_SCHEMA: JsonObject = {
    "type": "object",
    "properties": {
        "job_id": {"type": "string"},
        "cancelled": {"type": "boolean"},
        "job": {"type": ["object", "null"]},
    },
    "required": ["job_id", "cancelled", "job"],
    "additionalProperties": False,
}

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
        "outputSchema": RUN_OUTPUT_SCHEMA,
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
        "outputSchema": RUN_OUTPUT_SCHEMA,
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
        "outputSchema": EVENTS_OUTPUT_SCHEMA,
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
        "outputSchema": CANCEL_OUTPUT_SCHEMA,
        "annotations": {
            "readOnlyHint": False,
            "destructiveHint": True,
            "idempotentHint": True,
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
        enable_tasks: bool = False,
        task_ttl_ms: int = DEFAULT_TASK_TTL_MS,
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
        if not 1 <= task_ttl_ms <= DEFAULT_TASK_TTL_MS:
            raise ValueError("COOP_MCP_TASK_TTL_MS must be between 1 and 3600000")
        self.base_url = base_url
        self.api_key = api_key
        self.allowed_languages = allowed
        self.max_wait_seconds = max_wait_seconds
        self.max_code_bytes = max_code_bytes
        self.require_isolation = require_isolation
        self.enable_tasks = enable_tasks
        self.task_ttl_ms = task_ttl_ms

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
            enable_tasks=_env_bool("COOP_MCP_ENABLE_TASKS", False),
            task_ttl_ms=_env_int("COOP_MCP_TASK_TTL_MS", DEFAULT_TASK_TTL_MS),
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


class _McpProtocolError(Exception):
    def __init__(
        self, code: int, message: str, data: Optional[JsonValue] = None
    ) -> None:
        super().__init__(message)
        self.code = code
        self.message = message
        self.data = data


RequestId = Union[int, str]


class _RequestContext:
    def __init__(self, request_id: RequestId) -> None:
        self.request_id = request_id
        self.cancelled = threading.Event()
        self._lock = threading.Lock()
        self._job_id: Optional[str] = None
        self._committed = False

    def record_job(self, job_id: str) -> bool:
        with self._lock:
            self._job_id = job_id
            return self.cancelled.is_set()

    def cancel(self) -> Optional[str]:
        with self._lock:
            if self._committed:
                return None
            self.cancelled.set()
            return self._job_id

    def claim_response(self) -> bool:
        with self._lock:
            if self.cancelled.is_set():
                return False
            self._committed = True
            return True


class CoopMcpServer:
    """Dual-era MCP server whose tool handlers call one Coop tenant."""

    def __init__(self, config: McpConfig, client: Optional[Coop] = None) -> None:
        self.config = config
        self.client = client or Coop(config.base_url, config.api_key)
        self.initialized = False
        self._legacy_initialize_seen = False
        self._state_lock = threading.Lock()
        self._active_lock = threading.Lock()
        self._active: Dict[RequestId, _RequestContext] = {}

    @staticmethod
    def _server_info() -> JsonObject:
        return {"name": "coop-mcp", "version": __version__}

    @staticmethod
    def _instructions() -> str:
        return (
            "Use coop_run_code for short stateless jobs. Do not use it for "
            "persistent workspaces or interactive shells. The Coop URL, key, "
            "language allowlist, and isolation requirement are operator-controlled."
        )

    def _server_capabilities(self, *, modern: bool = False) -> JsonObject:
        capabilities: JsonObject = {"tools": {"listChanged": False}}
        if modern and self.config.enable_tasks:
            capabilities["extensions"] = {TASKS_EXTENSION: {}}
        return capabilities

    def begin_request(self, request_id: RequestId) -> Optional[_RequestContext]:
        with self._active_lock:
            if request_id in self._active:
                return None
            context = _RequestContext(request_id)
            self._active[request_id] = context
            return context

    def finish_request(self, request_id: RequestId) -> None:
        with self._active_lock:
            self._active.pop(request_id, None)

    def mark_request_cancelled(self, request_id: RequestId) -> Optional[str]:
        with self._active_lock:
            context = self._active.get(request_id)
        return None if context is None else context.cancel()

    def active_request_ids(self) -> List[RequestId]:
        with self._active_lock:
            return list(self._active)

    def cancel_job_for_request(self, request_id: RequestId) -> None:
        job_id = self.mark_request_cancelled(request_id)
        if job_id is None:
            return
        self.cancel_known_job(job_id)

    def cancel_known_job(self, job_id: str) -> None:
        try:
            self.client.cancel(job_id)
        except CoopError as exc:
            if exc.code != "job_already_terminal":
                raise

    def handle(
        self, message: JsonObject, context: Optional[_RequestContext] = None
    ) -> Optional[JsonObject]:
        request_id = message.get("id")
        is_notification = "id" not in message
        if message.get("jsonrpc") != "2.0" or not isinstance(
            message.get("method"), str
        ):
            if is_notification:
                return None
            return _rpc_error(request_id, -32600, "invalid JSON-RPC request")
        if not is_notification and not _valid_request_id(request_id):
            return _rpc_error(None, -32600, "request id must be a string or integer")

        method = cast(str, message["method"])
        params_value = message.get("params", {})
        if not isinstance(params_value, dict):
            if is_notification:
                return None
            return _rpc_error(request_id, -32602, "params must be an object")
        params = params_value

        if is_notification:
            if method == "notifications/initialized":
                with self._state_lock:
                    if self._legacy_initialize_seen:
                        self.initialized = True
            elif method == "notifications/cancelled":
                cancelled_id = params.get("requestId")
                if _valid_request_id(cancelled_id):
                    self.cancel_job_for_request(cast(RequestId, cancelled_id))
            return None

        if method == "initialize":
            requested = params.get("protocolVersion")
            protocol = (
                requested
                if isinstance(requested, str)
                and requested in SUPPORTED_PROTOCOL_VERSIONS
                else PROTOCOL_VERSION
            )
            with self._state_lock:
                self._legacy_initialize_seen = True
            return _rpc_result(
                request_id,
                {
                    "protocolVersion": protocol,
                    "capabilities": self._server_capabilities(modern=False),
                    "serverInfo": self._server_info(),
                    "instructions": self._instructions(),
                },
            )

        if method == "server/discover":
            try:
                self._modern_client_capabilities(params)
            except _McpProtocolError as exc:
                return _rpc_error(request_id, exc.code, exc.message, exc.data)
            return _rpc_result(
                request_id,
                {
                    "resultType": "complete",
                    "supportedVersions": [MODERN_PROTOCOL_VERSION],
                    "capabilities": self._server_capabilities(modern=True),
                    "instructions": self._instructions(),
                    "ttlMs": 0,
                    "cacheScope": "private",
                    "_meta": {
                        "io.modelcontextprotocol/serverInfo": self._server_info()
                    },
                },
            )

        modern = _has_modern_metadata(params)
        client_capabilities: Optional[JsonObject] = None
        if modern:
            try:
                client_capabilities = self._modern_client_capabilities(params)
            except _McpProtocolError as exc:
                return _rpc_error(request_id, exc.code, exc.message, exc.data)
        elif not self.initialized:
            return _rpc_error(request_id, -32002, "server is not initialized")
        if method == "ping":
            if modern:
                return _rpc_error(request_id, -32601, "method not found: ping")
            return _rpc_result(request_id, {})
        try:
            if method == "tools/list":
                result: JsonObject = {"tools": self._listed_tools(modern=modern)}
                if modern:
                    result["ttlMs"] = 0
                    result["cacheScope"] = "private"
                return _rpc_result(
                    request_id, self._complete_result(result) if modern else result
                )
            if method == "tools/call":
                result = self._call_tool(
                    params,
                    context=context,
                    client_capabilities=client_capabilities,
                    modern=modern,
                )
                if modern and result.get("resultType") != "task":
                    result = self._complete_result(result)
                elif modern:
                    self._attach_server_info(result)
                return _rpc_result(request_id, result)
            if method in {"tasks/get", "tasks/update", "tasks/cancel"}:
                if not modern:
                    return _rpc_error(request_id, -32601, f"method not found: {method}")
                self._require_tasks_capability(client_capabilities)
                if method == "tasks/get":
                    result = self._task_get(params)
                elif method == "tasks/update":
                    result = self._task_update(params)
                else:
                    result = self._task_cancel(params)
                return _rpc_result(request_id, self._complete_result(result))
        except _McpProtocolError as exc:
            return _rpc_error(request_id, exc.code, exc.message, exc.data)
        except CoopError as exc:
            if method.startswith("tasks/") and exc.code == "job_not_found":
                return _rpc_error(request_id, -32602, "unknown or expired taskId")
            return _rpc_error(request_id, -32603, self.redact(str(exc)))
        except (RuntimeError, TimeoutError, TypeError, ValueError) as exc:
            return _rpc_error(request_id, -32603, self.redact(str(exc)))
        return _rpc_error(request_id, -32601, f"method not found: {method}")

    def _attach_server_info(self, result: JsonObject) -> None:
        meta_value = result.get("_meta")
        meta = meta_value if isinstance(meta_value, dict) else {}
        meta["io.modelcontextprotocol/serverInfo"] = self._server_info()
        result["_meta"] = meta

    def _complete_result(self, result: JsonObject) -> JsonObject:
        completed = dict(result)
        completed["resultType"] = "complete"
        self._attach_server_info(completed)
        return completed

    def _modern_client_capabilities(self, params: JsonObject) -> JsonObject:
        meta = params.get("_meta")
        if not isinstance(meta, dict):
            raise _McpProtocolError(-32602, "modern requests require params._meta")
        requested = meta.get("io.modelcontextprotocol/protocolVersion")
        if requested != MODERN_PROTOCOL_VERSION:
            raise _McpProtocolError(
                -32022,
                "unsupported protocol version",
                {
                    "supported": [MODERN_PROTOCOL_VERSION],
                    "requested": str(requested),
                },
            )
        capabilities = meta.get("io.modelcontextprotocol/clientCapabilities")
        if not isinstance(capabilities, dict):
            raise _McpProtocolError(
                -32602,
                "modern requests require client capabilities in params._meta",
            )
        return capabilities

    def _client_supports_tasks(self, client_capabilities: Optional[JsonObject]) -> bool:
        if not self.config.enable_tasks or client_capabilities is None:
            return False
        extensions = client_capabilities.get("extensions")
        return isinstance(extensions, dict) and isinstance(
            extensions.get(TASKS_EXTENSION), dict
        )

    def _require_tasks_capability(
        self, client_capabilities: Optional[JsonObject]
    ) -> None:
        if not self.config.enable_tasks:
            raise _McpProtocolError(-32601, "tasks extension is disabled")
        if not self._client_supports_tasks(client_capabilities):
            raise _McpProtocolError(
                -32021,
                "missing required client capability",
                {"requiredCapabilities": {"extensions": {TASKS_EXTENSION: {}}}},
            )

    def _listed_tools(self, *, modern: bool = False) -> List[JsonValue]:
        """Return schemas narrowed to this operator-owned adapter policy."""
        capabilities = self.client.capabilities()
        tools: Any = json.loads(json.dumps(TOOLS))
        available_languages = sorted(
            self.config.allowed_languages.intersection(capabilities["languages"])
        )
        run_properties = tools[0]["inputSchema"]["properties"]
        run_properties["language"]["enum"] = available_languages
        tools[0]["inputSchema"]["properties"]["wait_seconds"]["maximum"] = (
            self.config.max_wait_seconds
        )
        tools[1]["inputSchema"]["properties"]["wait_seconds"]["maximum"] = (
            self.config.max_wait_seconds
        )
        limit_capabilities = capabilities["limits"]
        limit_properties = run_properties["limits"]["properties"]
        limit_properties["wall_seconds"]["maximum"] = limit_capabilities[
            "wall_seconds_max"
        ]
        limit_properties["cpu_seconds"]["maximum"] = limit_capabilities[
            "cpu_seconds_max"
        ]
        limit_properties["mem_mb"]["maximum"] = limit_capabilities["mem_mb_max"]
        limit_properties["max_pids"]["maximum"] = limit_capabilities["pids_max"]
        limit_properties["max_file_mb"]["maximum"] = limit_capabilities["file_mb_max"]
        max_code = min(self.config.max_code_bytes, limit_capabilities["code_bytes_max"])
        run_properties["code"]["description"] = (
            f"Complete source code for one short-lived job (at most {max_code} "
            "UTF-8 bytes)."
        )
        run_properties["stdin"]["description"] = (
            "Optional standard input, at most "
            f"{limit_capabilities['stdin_bytes_max']} UTF-8 bytes."
        )
        execution = capabilities["execution"]
        unsafe_host = not (
            execution["isolated"]
            and execution["private_rootfs"]
            and execution["dedicated_bootstrap"]
        )
        tools[0]["annotations"]["destructiveHint"] = unsafe_host
        tools[0]["annotations"]["openWorldHint"] = execution["networking"] != "disabled"
        posture = "isolated" if not unsafe_host else "UNISOLATED host"
        tools[0]["description"] = (
            f"Run one short job on the {posture} Coop backend and return its bounded "
            "output, terminal status, job ID, and receipt. Jobs are stateless; files "
            "and installed packages do not carry into the next isolated call."
        )
        if modern and self.config.enable_tasks:
            tools[0]["execution"] = {"taskSupport": "optional"}
        return cast(List[JsonValue], tools)

    def _call_tool(
        self,
        params: JsonObject,
        *,
        context: Optional[_RequestContext],
        client_capabilities: Optional[JsonObject],
        modern: bool,
    ) -> JsonObject:
        name = params.get("name")
        arguments_value = params.get("arguments", {})
        if not isinstance(name, str):
            raise _McpProtocolError(-32602, "tool name must be a string")
        if not isinstance(arguments_value, dict):
            raise _McpProtocolError(-32602, "tool arguments must be an object")
        arguments = arguments_value
        try:
            if name == "coop_run_code":
                if modern and self._client_supports_tasks(client_capabilities):
                    return self._run_code(arguments, context=context, create_task=True)
                return _tool_success(self._run_code(arguments, context=context))
            if name == "coop_job_result":
                return _tool_success(self._job_result(arguments, context=context))
            if name == "coop_job_events":
                return _tool_success(self._job_events(arguments))
            if name == "coop_cancel_job":
                return _tool_success(self._cancel_job(arguments))
            raise _McpProtocolError(-32602, f"unknown tool: {name}")
        except _McpProtocolError:
            raise
        except (CoopError, RuntimeError, TimeoutError, TypeError, ValueError) as exc:
            return _tool_error(self.redact(str(exc)))

    def _ensure_posture(self) -> Capabilities:
        # Capabilities shape schemas and provide early diagnostics. Security is
        # enforced atomically by the submit-time minimum-isolation requirement.
        return self.client.capabilities()

    def _result_with_context(
        self,
        job_id: str,
        timeout: float,
        context: Optional[_RequestContext],
    ) -> Mapping[str, Any]:
        if context is None:
            return self.client.result(job_id, timeout=timeout)
        deadline = time.monotonic() + timeout
        while True:
            if context.cancelled.is_set():
                raise TimeoutError("MCP request was cancelled")
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError(f"job {job_id} still running after {timeout}s")
            try:
                return self.client.result(job_id, timeout=min(1.0, remaining))
            except TimeoutError:
                continue
            except CoopError as exc:
                if exc.code != "request_timeout":
                    raise

    def _validate_limits(
        self, limits_value: JsonValue, capabilities: Capabilities
    ) -> Optional[Mapping[str, Any]]:
        if limits_value is None:
            return None
        if not isinstance(limits_value, dict):
            raise TypeError("limits must be an object")
        maxima = capabilities["limits"]
        bounds = {
            "wall_seconds": (1, maxima["wall_seconds_max"]),
            "cpu_seconds": (1, maxima["cpu_seconds_max"]),
            "mem_mb": (16, maxima["mem_mb_max"]),
            "max_pids": (8, maxima["pids_max"]),
            "max_file_mb": (1, maxima["file_mb_max"]),
        }
        unknown = set(limits_value).difference(set(bounds) | {"allow_network"})
        if unknown:
            raise TypeError(f"unknown limit: {', '.join(sorted(unknown))}")
        for name, (minimum, maximum) in bounds.items():
            value = limits_value.get(name)
            if value is None:
                continue
            if isinstance(value, bool) or not isinstance(value, int):
                raise TypeError(f"{name} must be an integer")
            if not minimum <= value <= maximum:
                raise ValueError(f"{name} must be between {minimum} and {maximum}")
        network = limits_value.get("allow_network")
        if network is not None and not isinstance(network, bool):
            raise TypeError("allow_network must be a boolean")
        if network is True:
            raise ValueError("allow_network=true is not supported by Coop policy")
        return cast(Mapping[str, Any], limits_value)

    def _run_code(
        self,
        arguments: JsonObject,
        *,
        context: Optional[_RequestContext] = None,
        create_task: bool = False,
    ) -> JsonObject:
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
        if isinstance(stdin_value, str):
            max_stdin = capabilities["limits"]["stdin_bytes_max"]
            if len(stdin_value.encode("utf-8")) > max_stdin:
                raise ValueError(
                    f"stdin exceeds the server limit of {max_stdin} UTF-8 bytes"
                )
        limits_value = arguments.get("limits")
        limits = self._validate_limits(limits_value, capabilities)
        wait_seconds = self._wait_seconds(arguments, default=60, allow_zero=False)

        submitted = self.client.submit(
            language,
            code,
            stdin=stdin_value,
            limits=limits,
            requirements=(
                {"minimum_isolation": "linux-shared-kernel"}
                if self.config.require_isolation
                else None
            ),
        )
        job_id = submitted["job_id"]
        if context is not None and context.record_job(job_id):
            try:
                self.client.cancel(job_id)
            except CoopError as exc:
                if exc.code != "job_already_terminal":
                    raise
        if create_task:
            now_ms = int(time.time() * 1000)
            return {
                "resultType": "task",
                "taskId": job_id,
                "status": "working",
                "statusMessage": "The Coop job was durably accepted.",
                "createdAt": _iso_timestamp(now_ms),
                "lastUpdatedAt": _iso_timestamp(now_ms),
                "ttlMs": self.config.task_ttl_ms,
                "pollIntervalMs": DEFAULT_TASK_POLL_INTERVAL_MS,
            }
        try:
            result = _json_object(
                self._result_with_context(job_id, wait_seconds, context)
            )
        except Exception as exc:
            snapshot: JsonObject = {}
            try:
                snapshot = _json_object(self.client.get(job_id))
            except Exception:
                pass
            failure: JsonObject = {
                "job_id": job_id,
                "status": snapshot.get("status", submitted.get("status")),
                "complete": False,
                "message": (
                    "The post-submit wait failed; the durable job remains addressable. "
                    "Call coop_job_result with this job_id or cancel it."
                ),
                "error_code": (
                    exc.code if isinstance(exc, CoopError) else type(exc).__name__
                ),
                "retryable": (
                    exc.retryable
                    if isinstance(exc, CoopError)
                    else isinstance(exc, TimeoutError)
                ),
            }
            return failure
        try:
            self._attach_evidence(result, job_id)
        except Exception as exc:
            result["evidence_error"] = self.redact(str(exc))
        result["complete"] = True
        return result

    def _task_base(self, snapshot: JsonObject) -> JsonObject:
        now_ms = int(time.time() * 1000)
        created_value = snapshot.get("created_at_ms")
        created_ms = created_value if isinstance(created_value, int) else now_ms
        timestamps = [created_ms]
        for name in ("started_at_ms", "finished_at_ms"):
            value = snapshot.get(name)
            if isinstance(value, int):
                timestamps.append(value)
        return {
            "taskId": str(snapshot.get("job_id", "")),
            "createdAt": _iso_timestamp(created_ms),
            "lastUpdatedAt": _iso_timestamp(max(timestamps)),
            "ttlMs": self.config.task_ttl_ms,
            "pollIntervalMs": DEFAULT_TASK_POLL_INTERVAL_MS,
        }

    def _task_get(self, params: JsonObject) -> JsonObject:
        _reject_unknown(params, {"taskId", "_meta"})
        task_id = _required_string(params, "taskId")
        snapshot = _json_object(self.client.get(task_id))
        task = self._task_base(snapshot)
        status = str(snapshot.get("status", ""))
        if status in {"queued", "running"}:
            task["status"] = "working"
            task["statusMessage"] = f"Coop job is {status}."
            return task
        if status == "cancelled":
            task["status"] = "cancelled"
            task["statusMessage"] = "Coop job cancellation reached a terminal state."
            return task
        if status not in {
            "succeeded",
            "failed",
            "timed_out",
            "oom_killed",
            "error",
        }:
            raise RuntimeError(f"unknown Coop job status: {status}")
        value = _json_object(self.client.result(task_id, timeout=5))
        try:
            self._attach_evidence(value, task_id)
        except Exception as exc:
            value["evidence_error"] = self.redact(str(exc))
        value["complete"] = True
        task["status"] = "completed"
        task["statusMessage"] = f"Coop job completed with status {status}."
        task["result"] = self._complete_result(
            _tool_result(value, is_error=status != "succeeded")
        )
        return task

    def _task_update(self, params: JsonObject) -> JsonObject:
        _reject_unknown(params, {"taskId", "inputResponses", "_meta"})
        task_id = _required_string(params, "taskId")
        responses = params.get("inputResponses")
        if not isinstance(responses, dict):
            raise _McpProtocolError(-32602, "inputResponses must be an object")
        # Coop execution jobs never request client input. Resolving the job is
        # still required so unknown or foreign task handles fail tenant-scoped.
        self.client.get(task_id)
        return {}

    def _task_cancel(self, params: JsonObject) -> JsonObject:
        _reject_unknown(params, {"taskId", "_meta"})
        task_id = _required_string(params, "taskId")
        try:
            self.client.cancel(task_id)
        except CoopError as exc:
            if exc.code != "job_already_terminal":
                raise
        return {}

    def _job_result(
        self,
        arguments: JsonObject,
        *,
        context: Optional[_RequestContext] = None,
    ) -> JsonObject:
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
            result = _json_object(
                self._result_with_context(job_id, wait_seconds, context)
            )
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
        # A current or legacy Coop server may acknowledge cancellation with an
        # empty HTTP 200. Reaching this line means the request was accepted.
        return {"job_id": job_id, "cancelled": True, "job": job}

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

    def redact(self, message: str) -> str:
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


def _valid_request_id(value: JsonValue) -> bool:
    return isinstance(value, str) or (
        isinstance(value, int) and not isinstance(value, bool)
    )


def _has_modern_metadata(params: JsonObject) -> bool:
    meta = params.get("_meta")
    return isinstance(meta, dict) and (
        "io.modelcontextprotocol/protocolVersion" in meta
    )


def _iso_timestamp(epoch_ms: int) -> str:
    value = datetime.fromtimestamp(epoch_ms / 1000, tz=timezone.utc)
    return value.isoformat(timespec="milliseconds").replace("+00:00", "Z")


def _rpc_result(request_id: JsonValue, result: JsonObject) -> JsonObject:
    return {"jsonrpc": "2.0", "id": request_id, "result": result}


def _rpc_error(
    request_id: JsonValue, code: int, message: str, data: Optional[JsonValue] = None
) -> JsonObject:
    error: JsonObject = {"code": code, "message": message}
    if data is not None:
        error["data"] = data
    return {"jsonrpc": "2.0", "id": request_id, "error": error}


def _tool_result(value: JsonObject, *, is_error: bool) -> JsonObject:
    serialized = json.dumps(
        value, separators=(",", ":"), ensure_ascii=False, allow_nan=False
    )
    return {
        "content": [{"type": "text", "text": serialized}],
        "structuredContent": value,
        "isError": is_error,
    }


def _tool_success(value: JsonObject) -> JsonObject:
    return _tool_result(value, is_error=False)


def _tool_error(message: str) -> JsonObject:
    return {"content": [{"type": "text", "text": message}], "isError": True}


def _loads_json_line(line: str) -> Any:
    def reject_constant(value: str) -> None:
        raise ValueError(f"invalid JSON constant: {value}")

    return json.loads(line, parse_constant=reject_constant)


def serve(
    server: CoopMcpServer,
    input_stream: TextIO = sys.stdin,
    output_stream: TextIO = sys.stdout,
) -> None:
    """Serve concurrent newline-delimited MCP JSON-RPC until stdin closes."""
    write_lock = threading.Lock()
    slots = threading.BoundedSemaphore(MAX_CONCURRENT_REQUESTS)
    workers = concurrent.futures.ThreadPoolExecutor(
        max_workers=MAX_CONCURRENT_REQUESTS,
        thread_name_prefix="coop-mcp-request",
    )
    cancellations = concurrent.futures.ThreadPoolExecutor(
        max_workers=2,
        thread_name_prefix="coop-mcp-cancel",
    )
    shutting_down = threading.Event()
    output_failed = threading.Event()

    def write_response(response: JsonObject) -> None:
        if shutting_down.is_set():
            return
        encoded = json.dumps(
            response,
            separators=(",", ":"),
            ensure_ascii=False,
            allow_nan=False,
        )
        with write_lock:
            if shutting_down.is_set():
                return
            try:
                output_stream.write(encoded + "\n")
                output_stream.flush()
            except OSError:
                output_failed.set()
                shutting_down.set()

    def run_request(message: JsonObject, context: _RequestContext) -> None:
        try:
            if context.cancelled.is_set():
                return
            try:
                response = server.handle(message, context)
            except Exception as exc:
                response = _rpc_error(
                    context.request_id,
                    -32603,
                    server.redact(f"internal error: {exc}"),
                )
            if response is not None and context.claim_response():
                write_response(response)
        finally:
            server.finish_request(context.request_id)
            slots.release()

    try:
        for line in input_stream:
            if output_failed.is_set():
                break
            response: Optional[JsonObject] = None
            try:
                decoded = _loads_json_line(line)
                if not isinstance(decoded, dict):
                    response = _rpc_error(None, -32600, "request must be a JSON object")
                    write_response(response)
                    continue
                message = cast(JsonObject, decoded)
            except (json.JSONDecodeError, UnicodeError, ValueError) as exc:
                write_response(_rpc_error(None, -32700, f"parse error: {exc}"))
                continue

            method = message.get("method")
            is_notification = "id" not in message
            if (
                is_notification
                and message.get("jsonrpc") == "2.0"
                and method == "notifications/cancelled"
            ):
                params = message.get("params", {})
                if isinstance(params, dict):
                    target = params.get("requestId")
                    if _valid_request_id(target):
                        job_id = server.mark_request_cancelled(cast(RequestId, target))
                        if job_id is not None:
                            cancellations.submit(server.cancel_known_job, job_id)
                continue

            control_message = is_notification or method in {
                "initialize",
                "server/discover",
                "ping",
            }
            request_id = message.get("id")
            if control_message or not _valid_request_id(request_id):
                response = server.handle(message)
                if response is not None:
                    write_response(response)
                continue

            typed_id = cast(RequestId, request_id)
            if not slots.acquire(blocking=False):
                write_response(_rpc_error(typed_id, -32000, "server busy"))
                continue
            context = server.begin_request(typed_id)
            if context is None:
                slots.release()
                write_response(
                    _rpc_error(typed_id, -32600, "duplicate active request id")
                )
                continue
            workers.submit(run_request, message, context)
    finally:
        shutting_down.set()
        for request_id in server.active_request_ids():
            job_id = server.mark_request_cancelled(request_id)
            if job_id is not None:
                cancellations.submit(server.cancel_known_job, job_id)
        workers.shutdown(wait=True, cancel_futures=True)
        cancellations.shutdown(wait=True, cancel_futures=False)


def main() -> None:
    try:
        config = McpConfig.from_env()
        serve(CoopMcpServer(config))
    except (OSError, ValueError) as exc:
        print(f"coop-mcp: {exc}", file=sys.stderr)
        raise SystemExit(2) from exc


if __name__ == "__main__":
    main()
