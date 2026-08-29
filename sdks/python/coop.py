"""Dependency-free synchronous client for the Coop execution API.

The module is intentionally kept as one file: copy it into a project, or
install the small ``coop-sdk`` package. WebSocket streaming is used when the
optional ``websocket-client`` package is present; cursor polling is the safe
fallback.
"""

from __future__ import annotations

import http.client
import json
import math
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid
from dataclasses import asdict, dataclass
from email.utils import parsedate_to_datetime
from enum import Enum
from typing import (
    Any,
    Callable,
    Dict,
    FrozenSet,
    Iterator,
    List,
    Literal,
    Mapping,
    Optional,
    Tuple,
    TypedDict,
    Union,
    cast,
)

__all__ = [
    "Capabilities",
    "CancellationResponse",
    "Coop",
    "CoopError",
    "CoopEvent",
    "EventChainReceipt",
    "EventPage",
    "ExecutorOutputEvidence",
    "ExecutorStreamEvidence",
    "EffectiveJobSpec",
    "EffectiveLimits",
    "ExecutionRequirements",
    "ExecutionPolicy",
    "HashedCoopEvent",
    "IsolationClass",
    "Job",
    "JobDetail",
    "JobPage",
    "JobResult",
    "JobSpec",
    "JobStatus",
    "JobStatusValue",
    "JobView",
    "Limits",
    "LimitEnforcement",
    "MinimumIsolation",
    "OutputEvidence",
    "Receipt",
    "ReceiptLimits",
    "ResourceUsage",
    "StoredJobSpec",
    "StoredLimits",
    "SubmitResponse",
    "SubmitResult",
    "WhoAmI",
    "isolation_satisfies",
]

__version__ = "0.3.0"


class JobStatus(str, Enum):
    QUEUED = "queued"
    RUNNING = "running"
    SUCCEEDED = "succeeded"
    FAILED = "failed"
    TIMED_OUT = "timed_out"
    OOM_KILLED = "oom_killed"
    CANCELLED = "cancelled"
    ERROR = "error"


JobStatusValue = Literal[
    "queued",
    "running",
    "succeeded",
    "failed",
    "timed_out",
    "oom_killed",
    "cancelled",
    "error",
]


TERMINAL: FrozenSet[str] = frozenset(
    status.value
    for status in JobStatus
    if status not in (JobStatus.QUEUED, JobStatus.RUNNING)
)


@dataclass(frozen=True)
class Limits:
    """Requested execution limits. The server records the effective limits."""

    wall_seconds: Optional[int] = None
    cpu_seconds: Optional[int] = None
    mem_mb: Optional[int] = None
    max_pids: Optional[int] = None
    max_file_mb: Optional[int] = None
    allow_network: Optional[bool] = None

    def to_dict(self) -> Dict[str, Union[int, bool]]:
        return {key: value for key, value in asdict(self).items() if value is not None}


class _RequiredJobSpec(TypedDict):
    language: str
    code: str


class JobSpec(_RequiredJobSpec, total=False):
    stdin: str
    limits: Dict[str, Union[int, bool]]
    requirements: "ExecutionRequirements"


IsolationClass = Literal[
    "none",
    "linux-shared-kernel",
    "gvisor-application-kernel",
    "wasm-capability",
    "hardware-vm",
    "confidential-vm",
]
MinimumIsolation = IsolationClass


def isolation_satisfies(observed: IsolationClass, minimum: IsolationClass) -> bool:
    """Return whether an observed provider class satisfies a requested minimum."""

    if minimum == "none":
        return True
    if minimum == "wasm-capability":
        return observed == "wasm-capability"
    process_chain = {
        "none": 0,
        "linux-shared-kernel": 1,
        "gvisor-application-kernel": 2,
        "hardware-vm": 3,
        "confidential-vm": 4,
    }
    if observed == "wasm-capability":
        return False
    return process_chain[observed] >= process_chain[minimum]


class ExecutionRequirements(TypedDict, total=False):
    """Atomic admission requirements enforced by a supporting Coop server."""

    minimum_isolation: MinimumIsolation


class StoredLimits(TypedDict):
    """Complete limits recorded by the server after defaults are applied."""

    wall_seconds: int
    cpu_seconds: int
    mem_mb: int
    max_pids: int
    max_file_mb: int
    allow_network: bool


class _RequiredStoredJobSpec(TypedDict):
    """The complete requested spec returned by a job lookup."""

    language: str
    code: str
    stdin: Optional[str]
    limits: StoredLimits


class StoredJobSpec(_RequiredStoredJobSpec, total=False):
    requirements: ExecutionRequirements


class EffectiveLimits(TypedDict):
    """Effective controls; ``None`` means the control was not enforced."""

    wall_seconds: Optional[int]
    cpu_seconds: Optional[int]
    mem_mb: Optional[int]
    max_pids: Optional[int]
    max_file_mb: Optional[int]
    allow_network: Optional[bool]


class EffectiveJobSpec(TypedDict):
    language: str
    code: str
    stdin: Optional[str]
    limits: EffectiveLimits
    requirements: ExecutionRequirements
    isolation_class: Optional[IsolationClass]


class LimitEnforcement(TypedDict):
    wall_seconds: bool
    cpu_seconds: bool
    mem_mb: bool
    max_pids: bool
    max_file_mb: bool


class _RequiredSubmitResponse(TypedDict):
    job_id: str
    status: JobStatusValue
    stream_url: str
    replay_url: str


class SubmitResponse(_RequiredSubmitResponse, total=False):
    stream_ticket_url: str


class SubmitResult(TypedDict):
    job: SubmitResponse
    location: Optional[str]
    idempotency_replayed: bool


class JobView(TypedDict):
    job_id: str
    tenant: str
    language: str
    status: JobStatusValue
    created_at_ms: int
    started_at_ms: Optional[int]
    finished_at_ms: Optional[int]
    exit_code: Optional[int]


class CancellationResponse(TypedDict):
    """Normalized cancellation acknowledgement across current and legacy servers."""

    job: Optional[JobView]
    cancellation_requested: bool
    already_terminal: bool


class ExecutionPolicy(TypedDict):
    """Observed policy; fields are unknown for queued or migrated rows."""

    sandbox: Optional[str]
    isolation_class: Optional[IsolationClass]
    bootstrap_ready: Optional[bool]
    isolated: Optional[bool]
    seccomp: Optional[bool]
    network_allowed: Optional[bool]
    networking: Optional[Literal["disabled", "host"]]
    private_rootfs: Optional[bool]
    dedicated_bootstrap: Optional[bool]
    limit_enforcement: Optional[LimitEnforcement]
    runtime_version: Optional[str]
    runtime_sha256: Optional[str]
    rootfs_sha256: Optional[str]
    config_sha256: Optional[str]


class EventChainReceipt(TypedDict):
    version: int
    head: Optional[str]
    events: int
    event_count: int
    verified_events: int
    legacy_events: int
    complete: bool


class _RequiredOutputEvidence(TypedDict):
    encoding: Literal["utf8-event-lines-joined-by-lf-no-trailing-lf"]
    stdout_bytes: int
    stderr_bytes: int
    stdout_sha256: str
    stderr_sha256: str
    truncated: bool


class OutputEvidence(_RequiredOutputEvidence):
    """Digests of the canonical, durably persisted output event encoding."""


class ExecutorStreamEvidence(TypedDict):
    """Raw executor observations before bounded event-sink persistence."""

    bytes_seen: int
    bytes_offered_to_sink: int
    records_offered_to_sink: int
    raw_sha256: str
    executor_truncated: bool


class ExecutorOutputEvidence(TypedDict):
    stdout: ExecutorStreamEvidence
    stderr: ExecutorStreamEvidence


class ResourceUsage(TypedDict):
    wall_time_ms: int
    cpu_time_usec: Optional[int]
    memory_peak_bytes: Optional[int]


class ReceiptLimits(TypedDict, total=False):
    """Limits preserved in a receipt; migrated/recovery evidence can be partial."""

    wall_seconds: int
    cpu_seconds: int
    mem_mb: int
    max_pids: int
    max_file_mb: int
    allow_network: bool


class _RequiredReceipt(TypedDict):
    version: int
    job_id: str
    outcome: JobStatusValue
    exit_code: Optional[int]
    finished_at_ms: int
    duration_ms: int
    event_chain: EventChainReceipt
    receipt_sha256: str


class Receipt(_RequiredReceipt, total=False):
    """Receipt core plus execution fields absent on restart-recovery receipts."""

    terminal_reason: str
    killed_by: Optional[str]
    created_at_ms: int
    started_at_ms: Optional[int]
    backend: str
    minimum_isolation: MinimumIsolation
    isolation_class: IsolationClass
    bootstrap_ready: bool
    isolated: bool
    seccomp: bool
    network_allowed: Optional[bool]
    networking: Optional[Literal["disabled", "host"]]
    private_rootfs: bool
    dedicated_bootstrap: bool
    runtime_version: Optional[str]
    runtime_sha256: Optional[str]
    rootfs_sha256: Optional[str]
    config_sha256: Optional[str]
    evidence_complete: bool
    requested_limits: ReceiptLimits
    effective_limits: EffectiveLimits
    limit_enforcement: LimitEnforcement
    code_sha256: str
    stdin_sha256: str
    policy_sha256: str
    resource_usage: Optional[ResourceUsage]
    executor_output: Optional[ExecutorOutputEvidence]
    output: OutputEvidence


class JobDetail(JobView):
    requested_spec: StoredJobSpec
    effective_spec: Optional[EffectiveJobSpec]
    execution_policy: ExecutionPolicy
    receipt: Optional[Receipt]
    receipt_sha256: Optional[str]


Job = Union[JobView, JobDetail]


class _RequiredCoopEvent(TypedDict):
    seq: int
    ts_ms: int
    kind: str
    data: Dict[str, Any]


class CoopEvent(_RequiredCoopEvent, total=False):
    # v0.1 rows omit these keys or return nulls. Consumers must check before
    # treating an event as cryptographically linked evidence.
    prev_hash: Optional[str]
    event_hash: Optional[str]
    hash_version: Optional[int]


class HashedCoopEvent(_RequiredCoopEvent):
    """A v0.2 event after the caller has validated its evidence fields."""

    prev_hash: Optional[str]
    event_hash: str
    hash_version: Literal[1]


class JobResult(TypedDict):
    job_id: str
    status: JobStatusValue
    exit_code: Optional[int]
    duration_ms: Optional[int]
    stdout: str
    stderr: str
    truncated: bool
    violations: List[Dict[str, Any]]


class JobPage(TypedDict):
    items: List[JobView]
    next_cursor: Optional[str]


class EventPage(TypedDict):
    events: List[CoopEvent]
    next_cursor: Optional[int]


class StreamTicket(TypedDict):
    ticket: str
    stream_url: str
    expires_at_ms: int


class ExecutionCapabilities(TypedDict):
    backend: str
    isolation_class: IsolationClass
    isolated: bool
    private_rootfs: bool
    dedicated_bootstrap: bool
    seccomp: bool
    networking: Literal["disabled", "host"]
    limit_enforcement: LimitEnforcement


class LimitCapabilities(TypedDict):
    wall_seconds_max: int
    cpu_seconds_max: int
    mem_mb_max: int
    concurrent_mem_mb_max: int
    pids_max: int
    file_mb_max: int
    output_lines_max: int
    output_bytes_per_stream_max: int
    output_record_bytes_max: int
    code_bytes_max: int
    stdin_bytes_max: int


class FeatureCapabilities(TypedDict):
    result_wait: bool
    cancellation: bool
    event_cursors: bool
    stream_tickets: bool
    receipts: bool


class Capabilities(TypedDict):
    version: str
    languages: List[str]
    execution: ExecutionCapabilities
    limits: LimitCapabilities
    features: FeatureCapabilities


class WhoAmI(TypedDict):
    tenant: str


class CoopError(RuntimeError):
    """A structured API, protocol, or transport failure."""

    def __init__(
        self,
        message: str,
        *,
        status: Optional[int] = None,
        code: str = "unknown_error",
        request_id: Optional[str] = None,
        retryable: bool = False,
        body: str = "",
        retry_after: Optional[float] = None,
        idempotency_key: Optional[str] = None,
    ) -> None:
        super().__init__(message)
        self.message = message
        self.status = status
        self.code = code
        self.request_id = request_id
        self.retryable = retryable
        self.body = body
        self.retry_after = retry_after
        self.idempotency_key = idempotency_key

    def __str__(self) -> str:
        prefix = f"coop {self.status}" if self.status is not None else "coop"
        request = f" (request {self.request_id})" if self.request_id else ""
        return f"{prefix} [{self.code}]: {self.message}{request}"


LimitInput = Union[Limits, Mapping[str, Union[int, bool]]]
OpenUrl = Callable[..., Any]
WebSocketFactory = Callable[..., Any]


def _retry_after(headers: Mapping[str, str]) -> Optional[float]:
    raw = headers.get("Retry-After") or headers.get("retry-after")
    if not raw:
        return None
    try:
        return max(0.0, float(raw))
    except ValueError:
        try:
            return max(0.0, parsedate_to_datetime(raw).timestamp() - time.time())
        except (TypeError, ValueError, OverflowError):
            return None


def _error_from_response(
    status: int, body: str, headers: Mapping[str, str]
) -> CoopError:
    request_id = headers.get("x-request-id") or headers.get("X-Request-Id")
    code = f"http_{status}"
    message = body.strip() or f"HTTP {status}"
    retryable = status in (408, 425, 429) or status >= 500
    try:
        decoded_value: Any = json.loads(body)
        if isinstance(decoded_value, dict):
            decoded = cast(Dict[str, Any], decoded_value)
            envelope_value = decoded.get("error", decoded)
            envelope = (
                cast(Dict[str, Any], envelope_value)
                if isinstance(envelope_value, dict)
                else None
            )
            if envelope is not None:
                code = str(envelope.get("code") or code)
                message = str(
                    envelope.get("message") or envelope.get("detail") or message
                )
                request_id = (
                    str(
                        envelope.get("request_id")
                        or decoded.get("request_id")
                        or request_id
                        or ""
                    )
                    or None
                )
                if isinstance(envelope.get("retryable"), bool):
                    retryable = envelope["retryable"]
    except (json.JSONDecodeError, UnicodeError):
        pass
    return CoopError(
        message,
        status=status,
        code=code,
        request_id=request_id,
        retryable=retryable,
        body=body,
        retry_after=_retry_after(headers),
    )


class _SameOriginRedirect(urllib.request.HTTPRedirectHandler):
    """Prevent an HTTP redirect from forwarding a tenant key to another origin."""

    def __init__(self, base_url: str) -> None:
        super().__init__()
        parts = urllib.parse.urlsplit(base_url)
        port = parts.port or (443 if parts.scheme.lower() == "https" else 80)
        self._origin = (parts.scheme.lower(), parts.hostname, port)

    def redirect_request(
        self, req: Any, fp: Any, code: int, msg: str, headers: Any, newurl: str
    ) -> Any:
        target = urllib.parse.urlsplit(urllib.parse.urljoin(req.full_url, newurl))
        port = target.port or (443 if target.scheme.lower() == "https" else 80)
        origin = (target.scheme.lower(), target.hostname, port)
        if origin != self._origin:
            raise CoopError(
                "refused a cross-origin redirect that could expose the API key",
                status=code,
                code="unsafe_redirect",
                body=newurl,
            )
        if req.has_header("Idempotency-key") and code in (301, 302, 303):
            raise CoopError(
                "refused a redirect that could change a keyed submission's method or body",
                status=code,
                code="unsafe_redirect",
                body=newurl,
            )
        return super().redirect_request(req, fp, code, msg, headers, newurl)


class Coop:
    """Synchronous Coop API client."""

    def __init__(
        self,
        base_url: str,
        api_key: str,
        *,
        timeout: float = 30.0,
        opener: Optional[OpenUrl] = None,
        websocket_factory: Optional[WebSocketFactory] = None,
    ) -> None:
        parts = urllib.parse.urlsplit(base_url)
        if parts.scheme not in ("http", "https") or not parts.netloc:
            raise ValueError("base_url must be an absolute http:// or https:// URL")
        if parts.username or parts.password or parts.query or parts.fragment:
            raise ValueError(
                "base_url must not contain credentials, a query, or a fragment"
            )
        if not api_key.strip():
            raise ValueError("api_key must not be empty")
        if isinstance(timeout, bool) or not math.isfinite(timeout) or timeout <= 0:
            raise ValueError("timeout must be finite and positive")
        base_path = parts.path.rstrip("/")
        self.base_url = urllib.parse.urlunsplit(
            (parts.scheme, parts.netloc, base_path, "", "")
        )
        self.api_key = api_key
        self.timeout = timeout
        self._opener = (
            opener
            or urllib.request.build_opener(_SameOriginRedirect(self.base_url)).open
        )
        self._websocket_factory = websocket_factory

    def _url(
        self,
        path: str,
        query: Optional[Mapping[str, Union[str, int, None]]] = None,
    ) -> str:
        url = self.base_url + "/" + path.lstrip("/")
        if query:
            values = {key: value for key, value in query.items() if value is not None}
            if values:
                url += "?" + urllib.parse.urlencode(values)
        return url

    @staticmethod
    def _job_path(job_id: str) -> str:
        if not job_id:
            raise ValueError("job_id must not be empty")
        return "/v1/jobs/" + urllib.parse.quote(job_id, safe="")

    def _request(
        self,
        method: str,
        path: str,
        payload: Optional[Mapping[str, Any]] = None,
        *,
        query: Optional[Mapping[str, Union[str, int, None]]] = None,
        timeout: Optional[float] = None,
        idempotency_key: Optional[str] = None,
    ) -> Any:
        value, _, _ = self._request_with_metadata(
            method,
            path,
            payload,
            query=query,
            timeout=timeout,
            idempotency_key=idempotency_key,
        )
        return value

    def _request_with_metadata(
        self,
        method: str,
        path: str,
        payload: Optional[Mapping[str, Any]] = None,
        *,
        query: Optional[Mapping[str, Union[str, int, None]]] = None,
        timeout: Optional[float] = None,
        idempotency_key: Optional[str] = None,
    ) -> Tuple[Any, Mapping[str, str], Optional[int]]:
        data = (
            json.dumps(payload, separators=(",", ":")).encode("utf-8")
            if payload is not None
            else None
        )
        headers = {
            "Accept": "application/json",
            "Authorization": f"Bearer {self.api_key}",
            "User-Agent": f"coop-python/{__version__}",
        }
        if data is not None:
            headers["Content-Type"] = "application/json"
        if idempotency_key is not None:
            headers["Idempotency-Key"] = idempotency_key
        request = urllib.request.Request(
            self._url(path, query), data=data, method=method, headers=headers
        )
        try:
            with self._opener(
                request,
                timeout=self.timeout if timeout is None else timeout,
            ) as response:
                raw = response.read()
                response_headers = cast(
                    Mapping[str, str], getattr(response, "headers", {})
                )
                response_status = cast(Optional[int], getattr(response, "status", None))
                if not raw:
                    return None, response_headers, response_status
                try:
                    return (
                        json.loads(raw.decode("utf-8")),
                        response_headers,
                        response_status,
                    )
                except (json.JSONDecodeError, UnicodeError) as exc:
                    raise CoopError(
                        "server returned invalid JSON",
                        status=getattr(response, "status", None),
                        code="invalid_response",
                        body=raw.decode("utf-8", errors="replace"),
                    ) from exc
        except urllib.error.HTTPError as exc:
            response_headers = cast(Mapping[str, str], exc.headers)
            try:
                try:
                    body = exc.read().decode("utf-8", errors="replace")
                except (
                    http.client.HTTPException,
                    urllib.error.URLError,
                    TimeoutError,
                    OSError,
                ) as read_exc:
                    reason = getattr(read_exc, "reason", read_exc)
                    request_id = response_headers.get(
                        "x-request-id"
                    ) or response_headers.get("X-Request-Id")
                    raise CoopError(
                        f"failed to read HTTP {exc.code} response body: {reason}",
                        status=exc.code,
                        code="transport_error",
                        request_id=request_id,
                        retryable=True,
                        retry_after=_retry_after(response_headers),
                    ) from read_exc
            finally:
                exc.close()
            raise _error_from_response(exc.code, body, response_headers) from None
        except (
            http.client.HTTPException,
            urllib.error.URLError,
            TimeoutError,
            OSError,
        ) as exc:
            reason = getattr(exc, "reason", exc)
            code = (
                "request_timeout"
                if isinstance(reason, TimeoutError)
                else "transport_error"
            )
            raise CoopError(str(reason), code=code, retryable=True) from exc

    @staticmethod
    def _limits_dict(
        limits: Optional[LimitInput], overrides: Mapping[str, Any]
    ) -> Dict[str, Any]:
        allowed = {
            "wall_seconds",
            "cpu_seconds",
            "mem_mb",
            "max_pids",
            "max_file_mb",
            "allow_network",
        }
        if limits is None:
            values: Dict[str, Any] = {}
        elif isinstance(limits, Limits):
            values = limits.to_dict()
        else:
            values = dict(limits)
        duplicate = set(values).intersection(overrides)
        if duplicate:
            raise ValueError(f"limit specified twice: {', '.join(sorted(duplicate))}")
        values.update(overrides)
        unknown = set(values).difference(allowed)
        if unknown:
            raise TypeError(f"unknown limit: {', '.join(sorted(unknown))}")
        return values

    def submit(
        self,
        language: str,
        code: str,
        stdin: Optional[str] = None,
        limits: Optional[LimitInput] = None,
        *,
        requirements: Optional[ExecutionRequirements] = None,
        idempotency_key: Optional[str] = None,
        retry_ambiguous: bool = False,
        max_ambiguous_retries: int = 1,
        retry_backoff: float = 0.25,
        **limit_overrides: Union[int, bool],
    ) -> SubmitResponse:
        """Submit a job and return the compatibility response body."""

        return self.submit_result(
            language,
            code,
            stdin,
            limits,
            requirements=requirements,
            idempotency_key=idempotency_key,
            retry_ambiguous=retry_ambiguous,
            max_ambiguous_retries=max_ambiguous_retries,
            retry_backoff=retry_backoff,
            **limit_overrides,
        )["job"]

    def submit_result(
        self,
        language: str,
        code: str,
        stdin: Optional[str] = None,
        limits: Optional[LimitInput] = None,
        *,
        requirements: Optional[ExecutionRequirements] = None,
        idempotency_key: Optional[str] = None,
        retry_ambiguous: bool = False,
        max_ambiguous_retries: int = 1,
        retry_backoff: float = 0.25,
        **limit_overrides: Union[int, bool],
    ) -> SubmitResult:
        """Submit a job with response metadata and optional ambiguous retry.

        Ambiguous retries are disabled by default. Opting in reuses one caller-
        supplied or generated key and assumes the target Coop server implements
        ``Idempotency-Key`` replay semantics for an identical submission.
        """
        if retry_ambiguous and idempotency_key is None:
            idempotency_key = str(uuid.uuid4())
        if idempotency_key is not None:
            key_value: Any = idempotency_key
            if (
                not isinstance(key_value, str)
                or not key_value
                or len(key_value) > 128
                or any(ord(char) < 0x21 or ord(char) > 0x7E for char in key_value)
            ):
                raise ValueError(
                    "idempotency_key must contain 1-128 visible ASCII bytes"
                )
            idempotency_key = key_value
        retry_value: Any = retry_ambiguous
        if not isinstance(retry_value, bool):
            raise TypeError("retry_ambiguous must be a boolean")
        retries_value: Any = max_ambiguous_retries
        if isinstance(retries_value, bool) or not isinstance(retries_value, int):
            raise TypeError("max_ambiguous_retries must be an integer")
        if not 0 <= max_ambiguous_retries <= 10:
            raise ValueError("max_ambiguous_retries must be between 0 and 10")
        backoff_value: Any = retry_backoff
        if (
            isinstance(backoff_value, bool)
            or not isinstance(backoff_value, (int, float))
            or not math.isfinite(float(retry_backoff))
            or retry_backoff < 0
            or retry_backoff > 60
        ):
            raise ValueError("retry_backoff must be finite and between 0 and 60")
        spec: JobSpec = {"language": language, "code": code}
        if stdin is not None:
            spec["stdin"] = stdin
        limit_values = self._limits_dict(limits, limit_overrides)
        if limit_values:
            spec["limits"] = cast(Dict[str, Union[int, bool]], limit_values)
        if requirements is not None:
            requirements_value: Any = requirements
            if not isinstance(requirements_value, dict):
                raise TypeError("requirements must be an object")
            requirements_mapping = cast(Dict[str, Any], requirements_value)
            unknown_requirements = set(requirements_mapping).difference(
                {"minimum_isolation"}
            )
            if unknown_requirements:
                raise TypeError(
                    "unknown requirement: " + ", ".join(sorted(unknown_requirements))
                )
            minimum: Any = requirements_mapping.get("minimum_isolation", "none")
            allowed_isolation = {
                "none",
                "linux-shared-kernel",
                "gvisor-application-kernel",
                "wasm-capability",
                "hardware-vm",
                "confidential-vm",
            }
            if not isinstance(minimum, str) or minimum not in allowed_isolation:
                raise ValueError("minimum_isolation is not a supported isolation class")
            spec["requirements"] = cast(
                ExecutionRequirements, dict(requirements_mapping)
            )
        attempt = 0
        while True:
            try:
                value, headers, _ = self._request_with_metadata(
                    "POST",
                    "/v1/jobs",
                    spec,
                    idempotency_key=idempotency_key,
                )
                replayed_raw = headers.get("idempotency-replayed") or headers.get(
                    "Idempotency-Replayed"
                )
                if replayed_raw is None:
                    replayed = False
                elif replayed_raw.lower() in {"true", "false"}:
                    replayed = replayed_raw.lower() == "true"
                else:
                    raise CoopError(
                        "server returned an invalid Idempotency-Replayed header",
                        code="invalid_response",
                    )
                location = headers.get("location") or headers.get("Location")
                return {
                    "job": cast(SubmitResponse, value),
                    "location": location,
                    "idempotency_replayed": replayed,
                }
            except CoopError as exc:
                if idempotency_key is not None:
                    exc.idempotency_key = idempotency_key
                ambiguous = exc.code in {"request_timeout", "transport_error"}
                if ambiguous and idempotency_key is None:
                    # Retrying an unkeyed accepted-or-not submission can create
                    # a duplicate job, so this failure is not safely retryable.
                    exc.retryable = False
                if (
                    not retry_ambiguous
                    or not ambiguous
                    or attempt >= max_ambiguous_retries
                ):
                    raise
                delay = float(retry_backoff) * (2**attempt)
                if exc.retry_after is not None:
                    delay = max(delay, min(exc.retry_after, 60.0))
                if delay > 0:
                    time.sleep(min(delay, 60.0))
                attempt += 1

    def _get_with_timeout(
        self, job_id: str, request_timeout: Optional[float]
    ) -> JobDetail:
        return cast(
            JobDetail,
            self._request(
                "GET",
                self._job_path(job_id),
                timeout=request_timeout,
            ),
        )

    def get(self, job_id: str) -> JobDetail:
        return self._get_with_timeout(job_id, None)

    def cancel_result(self, job_id: str) -> CancellationResponse:
        raw = self._request("DELETE", self._job_path(job_id))
        if raw is None:
            return {
                "job": None,
                "cancellation_requested": True,
                "already_terminal": False,
            }
        if not isinstance(raw, dict):
            raise CoopError("invalid cancellation response", code="invalid_response")
        response = cast(Dict[str, Any], raw)
        if "cancellation_requested" in response or "already_terminal" in response:
            requested = response.get("cancellation_requested")
            already_terminal = response.get("already_terminal")
            job_value = response.get("job")
            if (
                not isinstance(requested, bool)
                or not isinstance(already_terminal, bool)
                or (job_value is not None and not isinstance(job_value, dict))
            ):
                raise CoopError(
                    "invalid cancellation response", code="invalid_response"
                )
            return {
                "job": cast(Optional[JobView], job_value),
                "cancellation_requested": requested,
                "already_terminal": already_terminal,
            }
        # Legacy servers returned a JobView directly. A successful response is
        # an accepted cancellation even when its projected status has not yet
        # changed to ``cancelled``.
        return {
            "job": cast(JobView, response),
            "cancellation_requested": True,
            "already_terminal": False,
        }

    def cancel(self, job_id: str) -> Optional[JobView]:
        """Compatibility view of :meth:`cancel_result`, returning only the job."""

        return self.cancel_result(job_id)["job"]

    def whoami(self) -> WhoAmI:
        return cast(WhoAmI, self._request("GET", "/v1/whoami"))

    def capabilities(self) -> Capabilities:
        return cast(Capabilities, self._request("GET", "/v1/capabilities"))

    def list(
        self,
        *,
        limit: int = 50,
        cursor: Optional[str] = None,
        status: Optional[Union[JobStatus, str]] = None,
        language: Optional[str] = None,
    ) -> JobPage:
        if not 1 <= limit <= 500:
            raise ValueError("limit must be between 1 and 500")
        raw = self._request(
            "GET",
            "/v1/jobs",
            query={
                "limit": limit,
                "cursor": cursor,
                "status": status.value if isinstance(status, JobStatus) else status,
                "language": language,
            },
        )
        if isinstance(raw, list):
            return {"items": cast(List[JobView], raw), "next_cursor": None}
        if not isinstance(raw, dict):
            raise CoopError("invalid job list envelope", code="invalid_response")
        envelope = cast(Dict[str, Any], raw)
        items = envelope.get("items", envelope.get("jobs"))
        if not isinstance(items, list):
            raise CoopError("invalid job list envelope", code="invalid_response")
        return {
            "items": cast(List[JobView], items),
            "next_cursor": cast(Optional[str], envelope.get("next_cursor")),
        }

    def jobs(self, limit: int = 50) -> List[JobView]:
        """Compatibility shorthand returning only the first page."""
        return self.list(limit=limit)["items"]

    def event_page(
        self,
        job_id: str,
        *,
        after: Optional[int] = None,
        limit: int = 500,
    ) -> EventPage:
        return self._event_page_with_timeout(job_id, after, limit, None)

    def _event_page_with_timeout(
        self,
        job_id: str,
        after: Optional[int],
        limit: int,
        request_timeout: Optional[float],
    ) -> EventPage:
        if after is not None and after < -1:
            raise ValueError("after must be -1 or greater")
        if not 1 <= limit <= 5000:
            raise ValueError("limit must be between 1 and 5000")
        raw = self._request(
            "GET",
            self._job_path(job_id) + "/replay",
            query={
                "after": max(0, after) if after is not None else None,
                "limit": limit,
            },
            timeout=request_timeout,
        )
        if isinstance(raw, list):
            events = cast(List[CoopEvent], raw)
            if after is not None:
                events = [
                    event for event in events if int(event.get("seq", -1)) > after
                ]
            return {"events": events, "next_cursor": None}
        if not isinstance(raw, dict):
            raise CoopError("invalid event replay envelope", code="invalid_response")
        envelope = cast(Dict[str, Any], raw)
        if not isinstance(envelope.get("events"), list):
            raise CoopError("invalid event replay envelope", code="invalid_response")
        cursor = envelope.get("next_cursor")
        return {
            "events": cast(List[CoopEvent], envelope["events"]),
            "next_cursor": int(cursor) if cursor is not None else None,
        }

    def replay(
        self,
        job_id: str,
        *,
        after: Optional[int] = None,
        limit: int = 1000,
    ) -> List[CoopEvent]:
        events: List[CoopEvent] = []
        cursor = after
        while True:
            page = self.event_page(job_id, after=cursor, limit=limit)
            events.extend(page["events"])
            next_cursor = page["next_cursor"]
            if next_cursor is None:
                return events
            if cursor is not None and next_cursor <= cursor:
                raise CoopError("event cursor did not advance", code="invalid_response")
            cursor = next_cursor

    def wait(
        self,
        job_id: str,
        timeout: float = 60.0,
        poll_interval: float = 1.0,
    ) -> JobDetail:
        if not math.isfinite(timeout) or timeout < 0:
            raise ValueError("timeout must be finite and non-negative")
        if not math.isfinite(poll_interval) or poll_interval <= 0:
            raise ValueError("poll_interval must be finite and positive")
        deadline = time.monotonic() + timeout
        return self._wait_until(job_id, deadline, timeout, poll_interval)

    def _wait_until(
        self,
        job_id: str,
        deadline: float,
        timeout_label: float,
        poll_interval: float,
    ) -> JobDetail:
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError(f"job {job_id} still running after {timeout_label}s")
            view = self._get_with_timeout(job_id, remaining)
            if str(view.get("status")) in TERMINAL:
                return view
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError(f"job {job_id} still running after {timeout_label}s")
            time.sleep(min(poll_interval, remaining))

    def result(self, job_id: str, timeout: float = 60.0) -> JobResult:
        if not math.isfinite(timeout) or timeout < 0:
            raise ValueError("timeout must be finite and non-negative")
        deadline = time.monotonic() + timeout
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError(f"job {job_id} still running after {timeout}s")
            wait_seconds = min(300, int(math.floor(remaining)))
            try:
                view = cast(
                    JobResult,
                    self._request(
                        "GET",
                        self._job_path(job_id) + "/result",
                        query={"wait_seconds": wait_seconds},
                        timeout=remaining,
                    ),
                )
            except CoopError as exc:
                if exc.code not in ("http_404", "http_405"):
                    raise
                return self._result_via_polling(job_id, deadline)
            if str(view.get("status")) in TERMINAL:
                return view
            if time.monotonic() >= deadline:
                raise TimeoutError(f"job {job_id} still running after {timeout}s")
            time.sleep(min(1.0, max(0.0, deadline - time.monotonic())))

    def _result_via_polling(self, job_id: str, deadline: float) -> JobResult:
        wait_budget = deadline - time.monotonic()
        if wait_budget <= 0:
            raise TimeoutError(f"job {job_id} result deadline expired")
        view = self._wait_until(job_id, deadline, wait_budget, 1.0)
        stdout: List[str] = []
        stderr: List[str] = []
        truncated = False
        violations: List[Dict[str, Any]] = []
        after: Optional[int] = None
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError(f"job {job_id} result deadline expired")
            page = self._event_page_with_timeout(job_id, after, 500, remaining)
            for event in page["events"]:
                seq = int(event.get("seq", -1))
                after = max(after if after is not None else -1, seq)
                data = event.get("data") or {}
                kind = event.get("kind")
                if kind == "stdout":
                    stdout.append(str(data.get("line", "")))
                elif kind == "stderr":
                    stderr.append(str(data.get("line", "")))
                elif kind == "truncated":
                    truncated = True
                elif kind == "violation":
                    violations.append(data)
            if page["next_cursor"] is None:
                break
            after = page["next_cursor"]
        started_at_ms = view.get("started_at_ms")
        finished_at_ms = view.get("finished_at_ms")
        duration_ms = (
            finished_at_ms - started_at_ms
            if started_at_ms is not None and finished_at_ms is not None
            else None
        )
        return {
            "job_id": job_id,
            "status": view["status"],
            "exit_code": view.get("exit_code"),
            "duration_ms": duration_ms,
            "stdout": "\n".join(stdout),
            "stderr": "\n".join(stderr),
            "truncated": truncated,
            "violations": violations,
        }

    @staticmethod
    def _terminal_event(event: CoopEvent) -> bool:
        if event.get("kind") == "finished":
            return True
        return str((event.get("data") or {}).get("status", "")) in TERMINAL

    def _to_websocket_url(self, url: str) -> str:
        supplied = urllib.parse.urlsplit(url)
        absolute = url if supplied.scheme else self.base_url + "/" + url.lstrip("/")
        parts = urllib.parse.urlsplit(absolute)
        scheme = (
            "wss"
            if parts.scheme == "https"
            else "ws"
            if parts.scheme == "http"
            else parts.scheme
        )
        if scheme not in ("ws", "wss"):
            raise CoopError("invalid stream URL", code="invalid_response")
        return urllib.parse.urlunsplit(
            (scheme, parts.netloc, parts.path, parts.query, "")
        )

    def _stream_url(self, job_id: str, after: int, allow_legacy_query_key: bool) -> str:
        path = self._job_path(job_id)
        wire_after = max(0, after)
        try:
            ticket_value: Any = self._request("POST", path + "/stream-ticket")
        except CoopError as exc:
            if exc.status not in (404, 405):
                raise
            # A structured v0.2 error such as `job_not_found` is not evidence
            # that this endpoint is missing. Only an explicit opt-in plus an
            # unstructured legacy HTTP code may place a credential in a URL.
            legacy_endpoint_missing = exc.code in ("http_404", "http_405")
            if not allow_legacy_query_key or not legacy_endpoint_missing:
                raise CoopError(
                    "server does not support stream tickets and legacy key URLs are disabled",
                    code="stream_ticket_unavailable",
                ) from exc
            return self._to_websocket_url(
                self._url(path + "/stream", {"key": self.api_key, "after": wire_after})
            )
        if not isinstance(ticket_value, dict):
            raise CoopError("invalid stream ticket response", code="invalid_response")
        ticket_mapping = cast(Dict[str, Any], ticket_value)
        if not isinstance(ticket_mapping.get("stream_url"), str):
            raise CoopError("invalid stream ticket response", code="invalid_response")
        ticket = cast(StreamTicket, ticket_mapping)
        stream_url = self._to_websocket_url(ticket["stream_url"])
        parts = urllib.parse.urlsplit(stream_url)
        query = urllib.parse.parse_qsl(parts.query, keep_blank_values=True)
        if ticket.get("ticket") and not any(key == "ticket" for key, _ in query):
            query.append(("ticket", str(ticket["ticket"])))
        if not any(key == "after" for key, _ in query):
            query.append(("after", str(wire_after)))
        return urllib.parse.urlunsplit(
            (parts.scheme, parts.netloc, parts.path, urllib.parse.urlencode(query), "")
        )

    def _websocket_connect(self, url: str) -> Any:
        factory = self._websocket_factory
        if factory is None:
            try:
                import websocket  # type: ignore[import-not-found]
            except ImportError as exc:
                raise CoopError(
                    "install coop-sdk[stream] for WebSocket streaming",
                    code="websocket_unavailable",
                ) from exc
            factory = cast(WebSocketFactory, getattr(websocket, "create_connection"))
        return factory(
            url,
            timeout=self.timeout,
        )

    def stream(
        self,
        job_id: str,
        *,
        after: int = 0,
        prefer_websocket: bool = True,
        allow_legacy_query_key: bool = False,
        poll_interval: float = 1.0,
    ) -> Iterator[CoopEvent]:
        """Yield ordered events until the job reaches a terminal state."""
        after_value: Any = after
        interval_value: Any = poll_interval
        if (
            isinstance(after_value, bool)
            or not isinstance(after_value, int)
            or after < -1
            or isinstance(interval_value, bool)
            or not isinstance(interval_value, (int, float))
            or not math.isfinite(float(poll_interval))
            or poll_interval <= 0
        ):
            raise ValueError(
                "after must be an integer -1 or greater and poll_interval finite "
                "and positive"
            )
        cursor = after
        if prefer_websocket:
            socket = None
            try:
                socket = self._websocket_connect(
                    self._stream_url(job_id, cursor, allow_legacy_query_key)
                )
                while True:
                    raw = socket.recv()
                    if raw in (None, "", b""):
                        break
                    if isinstance(raw, bytes):
                        raw = raw.decode("utf-8")
                    event = cast(CoopEvent, json.loads(raw))
                    seq = int(event.get("seq", -1))
                    if seq <= cursor:
                        continue
                    cursor = seq
                    yield event
                    if self._terminal_event(event):
                        return
            except Exception:
                pass
            finally:
                if socket is not None:
                    try:
                        socket.close()
                    except Exception:
                        pass

        checks = 0
        terminal_projection_seen = False
        while True:
            page_limit = 500
            page = self.event_page(job_id, after=cursor, limit=page_limit)
            terminal_seen = False
            for event in page["events"]:
                seq = int(event.get("seq", -1))
                if seq <= cursor:
                    continue
                cursor = seq
                yield event
                if self._terminal_event(event):
                    terminal_seen = True
            # A v0.2 terminal event is the final durable row. Yield the rest
            # of a legacy replay page before stopping so compatibility data is
            # never silently dropped.
            if terminal_seen:
                return
            # A full page can have more durable history behind it. Drain
            # backlog without sleeping or consulting the (possibly already
            # terminal) job projection; otherwise a status check can cut off
            # output before the terminal event's page is reached.
            if len(page["events"]) >= page_limit:
                continue
            # A terminal projection is committed atomically with its terminal
            # event, but it may become visible between the replay and status
            # requests. Require one replay *after* observing that projection
            # before returning.
            if terminal_projection_seen:
                return
            checks += 1
            if checks % 5 == 0 and str(self.get(job_id).get("status")) in TERMINAL:
                terminal_projection_seen = True
                continue
            time.sleep(poll_interval)
