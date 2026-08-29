//! Privacy-bounded request correlation and W3C Trace Context parsing.
//!
//! Every ingress request starts a new local trace. A valid caller-supplied
//! `traceparent` is retained only as a fixed-size link to that new trace; it
//! never controls local sampling. Raw `tracestate` values are validated but
//! not retained, and baggage is deliberately not inspected.

use crate::metrics::{HttpMethod, HttpRoute};
use crate::AppState;
use axum::extract::{MatchedPath, Request, State};
use axum::http::{HeaderMap, HeaderValue};
use axum::middleware::Next;
use axum::response::Response;
use sha2::{Digest, Sha256};
use std::fmt;
use std::time::Instant;
use tracing::Instrument as _;
use uuid::Uuid;

pub(crate) const REQUEST_ID_HEADER: &str = "x-request-id";
pub(crate) const TRACEPARENT_HEADER: &str = "traceparent";
pub(crate) const TRACESTATE_HEADER: &str = "tracestate";

/// A defensive implementation limit for future-version `traceparent` fields.
///
/// W3C Trace Context permits implementations to reject prohibitively large
/// future-version fields. Version `00` remains exactly 55 bytes.
const MAX_TRACEPARENT_BYTES: usize = 512;

/// The documented amount of combined `tracestate` this trust boundary accepts.
///
/// This meets the W3C recommendation that vendors propagate at least 512
/// characters while placing a hard bound on attacker-controlled parsing work.
const MAX_TRACESTATE_BYTES: usize = 512;
const MAX_TRACESTATE_MEMBERS: usize = 32;
const MAX_TRACESTATE_KEY_BYTES: usize = 256;
const MAX_TRACESTATE_VALUE_BYTES: usize = 256;

const SAMPLED_FLAG: u8 = 0x01;
// W3C Trace Context Level 2 identifies bit 1 as the random-trace-id flag.
const RANDOM_TRACE_ID_FLAG: u8 = 0x02;

tokio::task_local! {
    static CURRENT_REQUEST_CONTEXT: RequestContext;
}

/// A server-owned sampling choice. Caller trace flags are never converted into
/// this type, making the trust decision explicit at the integration point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LocalSamplingDecision {
    Drop,
    Record,
}

impl LocalSamplingDecision {
    fn trace_flags(self) -> u8 {
        let sampled = match self {
            Self::Drop => 0,
            Self::Record => SAMPLED_FLAG,
        };
        RANDOM_TRACE_ID_FLAG | sampled
    }
}

/// A fixed-size, valid W3C trace identifier.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct TraceId([u8; 16]);

impl TraceId {
    pub(crate) fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

impl fmt::Display for TraceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_lower_hex(formatter, &self.0)
    }
}

impl fmt::Debug for TraceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "TraceId({self})")
    }
}

/// A fixed-size, valid W3C span identifier.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct SpanId([u8; 8]);

impl SpanId {
    pub(crate) fn as_bytes(&self) -> &[u8; 8] {
        &self.0
    }
}

impl fmt::Display for SpanId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_lower_hex(formatter, &self.0)
    }
}

impl fmt::Debug for SpanId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "SpanId({self})")
    }
}

/// Low-cardinality reasons an incoming `traceparent` was rejected.
///
/// These variants are safe to use as metric labels or structured log values;
/// the untrusted header value is intentionally absent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TraceparentRejection {
    MultipleHeaders,
    NonAscii,
    TooLong,
    InvalidLength,
    InvalidVersion,
    ForbiddenVersion,
    InvalidDelimiter,
    InvalidTraceId,
    ZeroTraceId,
    InvalidParentId,
    ZeroParentId,
    InvalidTraceFlags,
    InvalidFutureVersion,
}

impl TraceparentRejection {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::MultipleHeaders => "multiple_headers",
            Self::NonAscii => "non_ascii",
            Self::TooLong => "too_long",
            Self::InvalidLength => "invalid_length",
            Self::InvalidVersion => "invalid_version",
            Self::ForbiddenVersion => "forbidden_version",
            Self::InvalidDelimiter => "invalid_delimiter",
            Self::InvalidTraceId => "invalid_trace_id",
            Self::ZeroTraceId => "zero_trace_id",
            Self::InvalidParentId => "invalid_parent_id",
            Self::ZeroParentId => "zero_parent_id",
            Self::InvalidTraceFlags => "invalid_trace_flags",
            Self::InvalidFutureVersion => "invalid_future_version",
        }
    }
}

/// Low-cardinality reasons an incoming `tracestate` was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TracestateRejection {
    NonAscii,
    TooLong,
    TooManyMembers,
    MissingEquals,
    InvalidKey,
    DuplicateKey,
    InvalidValue,
}

impl TracestateRejection {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::NonAscii => "non_ascii",
            Self::TooLong => "too_long",
            Self::TooManyMembers => "too_many_members",
            Self::MissingEquals => "missing_equals",
            Self::InvalidKey => "invalid_key",
            Self::DuplicateKey => "duplicate_key",
            Self::InvalidValue => "invalid_value",
        }
    }
}

/// Validation summary for a companion `tracestate` header.
///
/// Only the number of non-empty entries is retained. Opaque vendor values can
/// contain sensitive information and are never copied into the context.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TracestateStatus {
    Absent,
    Valid { entry_count: u8 },
    Rejected(TracestateRejection),
}

/// A validated reference to an upstream trace at an untrusted boundary.
///
/// Coop always creates a fresh local trace rather than making this remote
/// context its parent. This structure is suitable for a tracing SDK `Link`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ExternalTraceLink {
    trace_id: TraceId,
    parent_span_id: SpanId,
    version: u8,
    tracestate: TracestateStatus,
}

impl ExternalTraceLink {
    pub(crate) fn trace_id(&self) -> TraceId {
        self.trace_id
    }

    pub(crate) fn parent_span_id(&self) -> SpanId {
        self.parent_span_id
    }

    pub(crate) fn version(&self) -> u8 {
        self.version
    }

    pub(crate) fn tracestate(&self) -> TracestateStatus {
        self.tracestate
    }
}

/// Safe result of processing caller-supplied trace headers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IncomingTrace {
    Absent,
    Rejected(TraceparentRejection),
    /// A `tracestate` without `traceparent` is invalid and discarded.
    OrphanTracestate,
    Linked(ExternalTraceLink),
}

impl IncomingTrace {
    /// A bounded label suitable for telemetry. Detailed rejection enums remain
    /// available when a separate low-cardinality reason is useful.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Absent => "absent",
            Self::Rejected(_) => "rejected",
            Self::OrphanTracestate => "orphan_tracestate",
            Self::Linked(_) => "linked",
        }
    }
}

/// Correlation state generated exactly once at HTTP ingress and cheap to clone
/// into the in-memory job lifecycle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RequestContext {
    request_id: Uuid,
    trace_id: TraceId,
    span_id: SpanId,
    sampling: LocalSamplingDecision,
    incoming: IncomingTrace,
}

impl RequestContext {
    /// Creates a privacy-bounded request context from HTTP request headers.
    ///
    /// `x-request-id` and `baggage` are deliberately ignored. A UUIDv7 request
    /// ID and a fresh local trace are generated for every invocation.
    pub(crate) fn from_headers(headers: &HeaderMap, sampling: LocalSamplingDecision) -> Self {
        let request_id = Uuid::now_v7();
        let (trace_id, span_id) = new_local_trace_ids(request_id);
        let incoming = parse_incoming_trace(headers);
        Self {
            request_id,
            trace_id,
            span_id,
            sampling,
            incoming,
        }
    }

    pub(crate) fn request_id(&self) -> Uuid {
        self.request_id
    }

    pub(crate) fn trace_id(&self) -> TraceId {
        self.trace_id
    }

    pub(crate) fn span_id(&self) -> SpanId {
        self.span_id
    }

    pub(crate) fn sampling(&self) -> LocalSamplingDecision {
        self.sampling
    }

    pub(crate) fn incoming(&self) -> IncomingTrace {
        self.incoming
    }

    #[allow(dead_code)] // Consumed by an optional future OpenTelemetry Link bridge.
    pub(crate) fn external_link(&self) -> Option<ExternalTraceLink> {
        match self.incoming {
            IncomingTrace::Linked(link) => Some(link),
            IncomingTrace::Absent
            | IncomingTrace::Rejected(_)
            | IncomingTrace::OrphanTracestate => None,
        }
    }

    /// W3C `traceparent` for downstream calls made by this local request span.
    /// It always uses the fresh local trace and the server-owned sampling bit.
    #[allow(dead_code)] // Narrow hook for a future trusted downstream client.
    pub(crate) fn local_traceparent(&self) -> String {
        format!(
            "00-{}-{}-{:02x}",
            self.trace_id,
            self.span_id,
            self.sampling.trace_flags()
        )
    }

    pub(crate) fn job_context(&self) -> JobTraceContext {
        JobTraceContext {
            request_id: self.request_id,
            trace_id: self.trace_id,
            parent_span_id: self.span_id,
            span_id: new_span_id(self.trace_id, self.span_id),
            run_span_id: new_span_id(self.trace_id, self.span_id),
            sampling: self.sampling,
            incoming: self.incoming,
        }
    }
}

/// Correlation copied into process-local job ownership. It is intentionally
/// absent from the durable schema: restart recovery creates a new local trace
/// until a future store migration can persist only these fixed-size IDs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct JobTraceContext {
    request_id: Uuid,
    trace_id: TraceId,
    parent_span_id: SpanId,
    /// Span for durable acceptance; the run span links beneath this value.
    span_id: SpanId,
    run_span_id: SpanId,
    sampling: LocalSamplingDecision,
    incoming: IncomingTrace,
}

impl JobTraceContext {
    #[allow(dead_code)] // Fixed-size persistence hook for a future store migration.
    pub(crate) fn request_id(&self) -> Uuid {
        self.request_id
    }

    #[allow(dead_code)]
    pub(crate) fn trace_id(&self) -> TraceId {
        self.trace_id
    }

    #[allow(dead_code)]
    pub(crate) fn run_span_id(&self) -> SpanId {
        self.run_span_id
    }

    #[allow(dead_code)]
    pub(crate) fn parent_span_id(&self) -> SpanId {
        self.parent_span_id
    }

    #[allow(dead_code)]
    pub(crate) fn incoming(&self) -> IncomingTrace {
        self.incoming
    }

    pub(crate) fn accept_span(&self, job_id: &str) -> tracing::Span {
        let request_id = self.request_id.to_string();
        let trace_id = self.trace_id.to_string();
        let span_id = self.span_id.to_string();
        let parent_span_id = self.parent_span_id.to_string();
        let trace_flags = format!("{:02x}", self.sampling.trace_flags());
        let (linked_trace_id, linked_span_id) = link_fields(self.incoming);
        tracing::info_span!(
            "coop.job.accept",
            request_id = %request_id,
            trace_id = %trace_id,
            span_id = %span_id,
            parent_span_id = %parent_span_id,
            trace_flags = %trace_flags,
            linked_trace_id = %linked_trace_id,
            linked_span_id = %linked_span_id,
            job_id = %job_id,
        )
    }

    pub(crate) fn record_on_current_job_span(&self) {
        let current = tracing::Span::current();
        current.record("request_id", tracing::field::display(self.request_id));
        current.record("trace_id", tracing::field::display(self.trace_id));
        current.record("span_id", tracing::field::display(self.run_span_id));
        current.record("parent_span_id", tracing::field::display(self.span_id));
        current.record(
            "trace_flags",
            tracing::field::display(format_args!("{:02x}", self.sampling.trace_flags())),
        );
        let (linked_trace_id, linked_span_id) = link_fields(self.incoming);
        current.record("linked_trace_id", tracing::field::display(linked_trace_id));
        current.record("linked_span_id", tracing::field::display(linked_span_id));
    }
}

pub(crate) fn current_request_id() -> Option<String> {
    CURRENT_REQUEST_CONTEXT
        .try_with(|context| context.request_id.to_string())
        .ok()
}

pub(crate) fn current_job_context() -> JobTraceContext {
    CURRENT_REQUEST_CONTEXT
        .try_with(RequestContext::job_context)
        .unwrap_or_else(|_| {
            RequestContext::from_headers(&HeaderMap::new(), LocalSamplingDecision::Record)
                .job_context()
        })
}

pub(crate) async fn middleware(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let sampling = match request.uri().path() {
        "/healthz" | "/readyz" | "/metrics" => LocalSamplingDecision::Drop,
        _ => LocalSamplingDecision::Record,
    };
    let context = RequestContext::from_headers(request.headers(), sampling);
    let method = HttpMethod::classify(request.method());
    let route = HttpRoute::classify(
        request
            .extensions()
            .get::<MatchedPath>()
            .map(MatchedPath::as_str),
    );
    let request_id = context.request_id().to_string();
    let trace_id = context.trace_id().to_string();
    let span_id = context.span_id().to_string();
    let trace_flags = format!("{:02x}", context.sampling().trace_flags());
    let incoming_trace = context.incoming().as_str();
    let incoming_rejection = incoming_rejection_reason(context.incoming());
    let (linked_trace_id, linked_span_id) = link_fields(context.incoming());
    let (linked_trace_version, tracestate_status, tracestate_rejection, tracestate_entries) =
        linked_trace_metadata(context.incoming());
    let span = tracing::info_span!(
        "http.request",
        request_id = %request_id,
        trace_id = %trace_id,
        span_id = %span_id,
        trace_flags = %trace_flags,
        linked_trace_id = %linked_trace_id,
        linked_span_id = %linked_span_id,
        incoming_trace,
        incoming_rejection,
        linked_trace_version,
        tracestate_status,
        tracestate_rejection,
        tracestate_entries,
        http.request.method = method.label(),
        http.route = route.label(),
        http.response.status_code = tracing::field::Empty,
        duration_ms = tracing::field::Empty,
    );
    request.extensions_mut().insert(context.clone());
    let observation = state.metrics.start_http(method, route);
    let started_at = Instant::now();
    let response = CURRENT_REQUEST_CONTEXT
        .scope(context, next.run(request).instrument(span.clone()))
        .await;
    let status = response.status();
    let duration_ms = started_at.elapsed().as_millis().min(u64::MAX as u128) as u64;
    span.record("http.response.status_code", status.as_u16());
    span.record("duration_ms", duration_ms);
    if status.is_server_error() {
        tracing::error!(parent: &span, status = status.as_u16(), duration_ms, "HTTP request completed");
    } else if status.is_client_error() {
        tracing::warn!(parent: &span, status = status.as_u16(), duration_ms, "HTTP request completed");
    } else if matches!(
        route,
        HttpRoute::Health | HttpRoute::Ready | HttpRoute::Metrics
    ) {
        tracing::debug!(parent: &span, status = status.as_u16(), duration_ms, "HTTP request completed");
    } else {
        tracing::info!(parent: &span, status = status.as_u16(), duration_ms, "HTTP request completed");
    }
    observation.finish(status.as_u16());

    let mut response = response;
    if let Ok(value) = HeaderValue::from_str(&request_id) {
        response.headers_mut().insert(REQUEST_ID_HEADER, value);
    }
    response
}

fn link_fields(incoming: IncomingTrace) -> (String, String) {
    match incoming {
        IncomingTrace::Linked(link) => (
            link.trace_id().to_string(),
            link.parent_span_id().to_string(),
        ),
        IncomingTrace::Absent | IncomingTrace::Rejected(_) | IncomingTrace::OrphanTracestate => {
            (String::new(), String::new())
        }
    }
}

fn incoming_rejection_reason(incoming: IncomingTrace) -> &'static str {
    match incoming {
        IncomingTrace::Rejected(reason) => reason.as_str(),
        IncomingTrace::OrphanTracestate => "orphan_tracestate",
        IncomingTrace::Absent | IncomingTrace::Linked(_) => "",
    }
}

fn linked_trace_metadata(incoming: IncomingTrace) -> (u64, &'static str, &'static str, u64) {
    let IncomingTrace::Linked(link) = incoming else {
        return (0, "absent", "", 0);
    };
    match link.tracestate() {
        TracestateStatus::Absent => (u64::from(link.version()), "absent", "", 0),
        TracestateStatus::Valid { entry_count } => (
            u64::from(link.version()),
            "valid",
            "",
            u64::from(entry_count),
        ),
        TracestateStatus::Rejected(reason) => {
            (u64::from(link.version()), "rejected", reason.as_str(), 0)
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ParsedTraceparent {
    trace_id: TraceId,
    parent_span_id: SpanId,
    version: u8,
}

fn parse_incoming_trace(headers: &HeaderMap) -> IncomingTrace {
    let mut traceparents = headers.get_all(TRACEPARENT_HEADER).iter();
    let Some(traceparent) = traceparents.next() else {
        return if headers.contains_key(TRACESTATE_HEADER) {
            IncomingTrace::OrphanTracestate
        } else {
            IncomingTrace::Absent
        };
    };
    if traceparents.next().is_some() {
        return IncomingTrace::Rejected(TraceparentRejection::MultipleHeaders);
    }

    let traceparent = match traceparent.to_str() {
        Ok(value) => value,
        Err(_) => return IncomingTrace::Rejected(TraceparentRejection::NonAscii),
    };
    let parsed = match parse_traceparent(traceparent) {
        Ok(parsed) => parsed,
        Err(reason) => return IncomingTrace::Rejected(reason),
    };

    let tracestate = parse_tracestate(headers);
    IncomingTrace::Linked(ExternalTraceLink {
        trace_id: parsed.trace_id,
        parent_span_id: parsed.parent_span_id,
        version: parsed.version,
        tracestate,
    })
}

fn parse_traceparent(value: &str) -> Result<ParsedTraceparent, TraceparentRejection> {
    let bytes = value.as_bytes();
    if bytes.len() > MAX_TRACEPARENT_BYTES {
        return Err(TraceparentRejection::TooLong);
    }
    if bytes.len() < 55 {
        return Err(TraceparentRejection::InvalidLength);
    }
    if !is_lower_hex(bytes[0]) || !is_lower_hex(bytes[1]) {
        return Err(TraceparentRejection::InvalidVersion);
    }
    if bytes[2] != b'-' || bytes[35] != b'-' || bytes[52] != b'-' {
        return Err(TraceparentRejection::InvalidDelimiter);
    }

    let version = decode_byte(bytes[0], bytes[1])
        .expect("version characters were validated as lowercase hex");
    if version == u8::MAX {
        return Err(TraceparentRejection::ForbiddenVersion);
    }
    if version == 0 && bytes.len() != 55 {
        return Err(TraceparentRejection::InvalidLength);
    }
    if version > 0 && bytes.len() > 55 && (bytes[55] != b'-' || bytes.len() == 56) {
        return Err(TraceparentRejection::InvalidFutureVersion);
    }

    let trace_id =
        decode_hex_array::<16>(&bytes[3..35]).ok_or(TraceparentRejection::InvalidTraceId)?;
    if trace_id.iter().all(|byte| *byte == 0) {
        return Err(TraceparentRejection::ZeroTraceId);
    }

    let parent_span_id =
        decode_hex_array::<8>(&bytes[36..52]).ok_or(TraceparentRejection::InvalidParentId)?;
    if parent_span_id.iter().all(|byte| *byte == 0) {
        return Err(TraceparentRejection::ZeroParentId);
    }

    decode_byte(bytes[53], bytes[54]).ok_or(TraceparentRejection::InvalidTraceFlags)?;

    Ok(ParsedTraceparent {
        trace_id: TraceId(trace_id),
        parent_span_id: SpanId(parent_span_id),
        version,
    })
}

fn parse_tracestate(headers: &HeaderMap) -> TracestateStatus {
    let mut values = headers.get_all(TRACESTATE_HEADER).iter();
    let Some(first) = values.next() else {
        return TracestateStatus::Absent;
    };

    let mut combined = String::new();
    if append_tracestate_value(&mut combined, first, false).is_err() {
        return tracestate_header_error(first);
    }
    for value in values {
        if append_tracestate_value(&mut combined, value, true).is_err() {
            return tracestate_header_error(value);
        }
    }

    match validate_tracestate(&combined) {
        Ok(entry_count) => TracestateStatus::Valid { entry_count },
        Err(reason) => TracestateStatus::Rejected(reason),
    }
}

fn append_tracestate_value(
    combined: &mut String,
    value: &axum::http::HeaderValue,
    add_separator: bool,
) -> Result<(), ()> {
    let value = value.to_str().map_err(|_| ())?;
    let added = value.len() + usize::from(add_separator);
    if combined.len().saturating_add(added) > MAX_TRACESTATE_BYTES {
        return Err(());
    }
    if add_separator {
        combined.push(',');
    }
    combined.push_str(value);
    Ok(())
}

fn tracestate_header_error(value: &axum::http::HeaderValue) -> TracestateStatus {
    if value.to_str().is_err() {
        TracestateStatus::Rejected(TracestateRejection::NonAscii)
    } else {
        TracestateStatus::Rejected(TracestateRejection::TooLong)
    }
}

fn validate_tracestate(value: &str) -> Result<u8, TracestateRejection> {
    let mut keys: Vec<&str> = Vec::new();
    let mut list_members = 0usize;

    for raw_member in value.split(',') {
        list_members += 1;
        if list_members > MAX_TRACESTATE_MEMBERS {
            return Err(TracestateRejection::TooManyMembers);
        }

        let member = trim_ows(raw_member);
        if member.is_empty() {
            return Err(TracestateRejection::MissingEquals);
        }

        let (key, member_value) = member
            .split_once('=')
            .ok_or(TracestateRejection::MissingEquals)?;
        if !valid_tracestate_key(key) {
            return Err(TracestateRejection::InvalidKey);
        }
        if keys.contains(&key) {
            return Err(TracestateRejection::DuplicateKey);
        }
        if !valid_tracestate_value(member_value) {
            return Err(TracestateRejection::InvalidValue);
        }
        keys.push(key);
    }

    Ok(keys.len() as u8)
}

fn valid_tracestate_key(key: &str) -> bool {
    let bytes = key.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_TRACESTATE_KEY_BYTES {
        return false;
    }
    let valid_key_char = |byte: u8| {
        is_lower_alpha(byte) || byte.is_ascii_digit() || matches!(byte, b'_' | b'-' | b'*' | b'/')
    };
    match key.split_once('@') {
        None => is_lower_alpha(bytes[0]) && bytes[1..].iter().copied().all(valid_key_char),
        Some((tenant, system)) => {
            !system.contains('@')
                && !tenant.is_empty()
                && tenant.len() <= 241
                && tenant
                    .as_bytes()
                    .first()
                    .is_some_and(|byte| is_lower_alpha(*byte) || byte.is_ascii_digit())
                && tenant
                    .as_bytes()
                    .iter()
                    .skip(1)
                    .copied()
                    .all(valid_key_char)
                && !system.is_empty()
                && system.len() <= 14
                && system
                    .as_bytes()
                    .first()
                    .is_some_and(|byte| is_lower_alpha(*byte))
                && system
                    .as_bytes()
                    .iter()
                    .skip(1)
                    .copied()
                    .all(valid_key_char)
        }
    }
}

fn valid_tracestate_value(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_TRACESTATE_VALUE_BYTES {
        return false;
    }
    if bytes
        .iter()
        .any(|byte| !(0x20..=0x7e).contains(byte) || matches!(*byte, b',' | b'='))
    {
        return false;
    }
    bytes
        .last()
        .is_some_and(|byte| *byte != b' ' && *byte != b'\t')
}

fn trim_ows(value: &str) -> &str {
    value.trim_matches([' ', '\t'])
}

fn new_local_trace_ids(request_id: Uuid) -> (TraceId, SpanId) {
    // UUIDv7 contributes cryptographic randomness but embeds a timestamp. A
    // domain-separated SHA-256 derivation hides that layout and yields uniform
    // bytes for the W3C random-trace-id flag without adding another RNG crate.
    let entropy = Uuid::now_v7();
    let mut digest = Sha256::new();
    digest.update(b"coop-request-context-v1\0");
    digest.update(request_id.as_bytes());
    digest.update(entropy.as_bytes());
    let digest = digest.finalize();

    let mut trace_id = [0u8; 16];
    trace_id.copy_from_slice(&digest[..16]);
    let mut span_id = [0u8; 8];
    span_id.copy_from_slice(&digest[16..24]);

    // These branches are vanishingly unlikely, but W3C explicitly forbids
    // all-zero identifiers, so construction enforces the invariant.
    if trace_id.iter().all(|byte| *byte == 0) {
        trace_id[15] = 1;
    }
    if span_id.iter().all(|byte| *byte == 0) {
        span_id[7] = 1;
    }
    (TraceId(trace_id), SpanId(span_id))
}

fn new_span_id(trace_id: TraceId, parent_span_id: SpanId) -> SpanId {
    let entropy = Uuid::now_v7();
    let mut digest = Sha256::new();
    digest.update(b"coop-job-span-v1\0");
    digest.update(trace_id.as_bytes());
    digest.update(parent_span_id.as_bytes());
    digest.update(entropy.as_bytes());
    let digest = digest.finalize();
    let mut span_id = [0_u8; 8];
    span_id.copy_from_slice(&digest[..8]);
    if span_id.iter().all(|byte| *byte == 0) {
        span_id[7] = 1;
    }
    SpanId(span_id)
}

fn decode_hex_array<const N: usize>(value: &[u8]) -> Option<[u8; N]> {
    if value.len() != N * 2 {
        return None;
    }
    let mut decoded = [0u8; N];
    for (index, byte) in decoded.iter_mut().enumerate() {
        *byte = decode_byte(value[index * 2], value[index * 2 + 1])?;
    }
    Some(decoded)
}

fn decode_byte(high: u8, low: u8) -> Option<u8> {
    Some((decode_nibble(high)? << 4) | decode_nibble(low)?)
}

fn decode_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        _ => None,
    }
}

fn is_lower_hex(value: u8) -> bool {
    value.is_ascii_digit() || matches!(value, b'a'..=b'f')
}

fn is_lower_alpha(value: u8) -> bool {
    value.is_ascii_lowercase()
}

fn write_lower_hex(formatter: &mut fmt::Formatter<'_>, bytes: &[u8]) -> fmt::Result {
    for byte in bytes {
        write!(formatter, "{byte:02x}")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{header::HeaderName, HeaderValue};

    const VALID_TRACE_ID: &str = "4bf92f3577b34da6a3ce929d0e0e4736";
    const VALID_PARENT_ID: &str = "00f067aa0ba902b7";

    fn traceparent(version: &str, flags: &str) -> String {
        format!("{version}-{VALID_TRACE_ID}-{VALID_PARENT_ID}-{flags}")
    }

    fn headers_with_traceparent(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            TRACEPARENT_HEADER,
            HeaderValue::from_str(value).expect("test traceparent must be an HTTP header value"),
        );
        headers
    }

    #[test]
    fn every_ingress_gets_uuid_v7_and_a_fresh_local_trace() {
        let mut headers = headers_with_traceparent(&traceparent("00", "01"));
        headers.insert(
            REQUEST_ID_HEADER,
            HeaderValue::from_static("caller-controlled"),
        );
        headers.insert("baggage", HeaderValue::from_static("secret=do-not-copy"));

        let first = RequestContext::from_headers(&headers, LocalSamplingDecision::Drop);
        let second = RequestContext::from_headers(&headers, LocalSamplingDecision::Drop);

        assert_eq!(first.request_id().get_version_num(), 7);
        assert_eq!(second.request_id().get_version_num(), 7);
        assert_ne!(first.request_id(), second.request_id());
        assert_ne!(first.trace_id(), second.trace_id());
        assert_ne!(first.span_id(), second.span_id());
        assert_ne!(first.trace_id().to_string(), VALID_TRACE_ID);
        assert_ne!(first.request_id().to_string(), "caller-controlled");
        assert_eq!(first.local_traceparent().len(), 55);
        assert!(first.local_traceparent().ends_with("-02"));
    }

    #[test]
    fn caller_sampling_never_controls_local_sampling() {
        let caller_sampled = headers_with_traceparent(&traceparent("00", "01"));
        let dropped = RequestContext::from_headers(&caller_sampled, LocalSamplingDecision::Drop);
        assert!(dropped.local_traceparent().ends_with("-02"));

        let caller_not_sampled = headers_with_traceparent(&traceparent("00", "00"));
        let recorded =
            RequestContext::from_headers(&caller_not_sampled, LocalSamplingDecision::Record);
        assert!(recorded.local_traceparent().ends_with("-03"));
    }

    #[test]
    fn valid_remote_context_is_a_link_not_the_local_parent() {
        let headers = headers_with_traceparent(&traceparent("00", "03"));
        let context = RequestContext::from_headers(&headers, LocalSamplingDecision::Drop);
        let link = context.external_link().expect("valid context should link");

        assert_eq!(link.trace_id().to_string(), VALID_TRACE_ID);
        assert_eq!(link.parent_span_id().to_string(), VALID_PARENT_ID);
        assert_eq!(link.version(), 0);
        assert_eq!(link.tracestate(), TracestateStatus::Absent);
        assert_ne!(context.trace_id(), link.trace_id());
        assert_ne!(context.span_id(), link.parent_span_id());
    }

    #[test]
    fn request_context_is_cloneable_for_job_lifecycle_correlation() {
        let context =
            RequestContext::from_headers(&HeaderMap::new(), LocalSamplingDecision::Record);
        assert_eq!(context, context.clone());
        assert_eq!(context.sampling(), LocalSamplingDecision::Record);
        assert_eq!(context.incoming(), IncomingTrace::Absent);
        assert!(context.trace_id().as_bytes().iter().any(|byte| *byte != 0));
        assert!(context.span_id().as_bytes().iter().any(|byte| *byte != 0));
    }

    #[test]
    fn future_traceparent_version_accepts_only_a_w3c_base_and_extension_boundary() {
        let mut value = traceparent("01", "ff");
        value.push_str("-future-fields-are-opaque");
        let headers = headers_with_traceparent(&value);
        let context = RequestContext::from_headers(&headers, LocalSamplingDecision::Drop);
        assert_eq!(context.external_link().map(|link| link.version()), Some(1));

        let invalid = headers_with_traceparent(&format!(
            "{}future-without-delimiter",
            traceparent("01", "00")
        ));
        assert_eq!(
            RequestContext::from_headers(&invalid, LocalSamplingDecision::Drop).incoming(),
            IncomingTrace::Rejected(TraceparentRejection::InvalidFutureVersion)
        );

        let empty_extension = headers_with_traceparent(&format!("{}-", traceparent("01", "00")));
        assert_eq!(
            RequestContext::from_headers(&empty_extension, LocalSamplingDecision::Drop).incoming(),
            IncomingTrace::Rejected(TraceparentRejection::InvalidFutureVersion)
        );
    }

    #[test]
    fn malformed_traceparents_are_rejected_without_retaining_input() {
        let cases = [
            (
                "00-4bf92f3577b34da6a3ce929d0e0e473-00f067aa0ba902b7-01",
                TraceparentRejection::InvalidLength,
            ),
            (
                "g0-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
                TraceparentRejection::InvalidVersion,
            ),
            (
                "ff-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
                TraceparentRejection::ForbiddenVersion,
            ),
            (
                "00_4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
                TraceparentRejection::InvalidDelimiter,
            ),
            (
                "00-4BF92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
                TraceparentRejection::InvalidTraceId,
            ),
            (
                "00-00000000000000000000000000000000-00f067aa0ba902b7-01",
                TraceparentRejection::ZeroTraceId,
            ),
            (
                "00-4bf92f3577b34da6a3ce929d0e0e4736-00F067aa0ba902b7-01",
                TraceparentRejection::InvalidParentId,
            ),
            (
                "00-4bf92f3577b34da6a3ce929d0e0e4736-0000000000000000-01",
                TraceparentRejection::ZeroParentId,
            ),
            (
                "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-g1",
                TraceparentRejection::InvalidTraceFlags,
            ),
            (
                "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01-extra",
                TraceparentRejection::InvalidLength,
            ),
        ];

        for (value, expected) in cases {
            let context = RequestContext::from_headers(
                &headers_with_traceparent(value),
                LocalSamplingDecision::Drop,
            );
            assert_eq!(
                context.incoming(),
                IncomingTrace::Rejected(expected),
                "{value}"
            );
            assert!(context.external_link().is_none());
        }
    }

    #[test]
    fn multiple_traceparent_fields_and_orphan_tracestate_are_rejected() {
        let mut duplicated = HeaderMap::new();
        let name = HeaderName::from_static(TRACEPARENT_HEADER);
        duplicated.append(
            name.clone(),
            HeaderValue::from_static("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"),
        );
        duplicated.append(
            name,
            HeaderValue::from_static("00-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-bbbbbbbbbbbbbbbb-00"),
        );
        assert_eq!(
            RequestContext::from_headers(&duplicated, LocalSamplingDecision::Drop).incoming(),
            IncomingTrace::Rejected(TraceparentRejection::MultipleHeaders)
        );

        let mut orphan = HeaderMap::new();
        orphan.insert(TRACESTATE_HEADER, HeaderValue::from_static("vendor=value"));
        assert_eq!(
            RequestContext::from_headers(&orphan, LocalSamplingDecision::Drop).incoming(),
            IncomingTrace::OrphanTracestate
        );
    }

    #[test]
    fn oversized_and_non_ascii_trace_headers_fail_with_bounded_reasons() {
        let mut oversized = traceparent("01", "00");
        oversized.push('-');
        oversized.push_str(&"x".repeat(MAX_TRACEPARENT_BYTES));
        assert_eq!(
            RequestContext::from_headers(
                &headers_with_traceparent(&oversized),
                LocalSamplingDecision::Drop,
            )
            .incoming(),
            IncomingTrace::Rejected(TraceparentRejection::TooLong)
        );

        let mut non_ascii = HeaderMap::new();
        non_ascii.insert(
            TRACEPARENT_HEADER,
            HeaderValue::from_bytes(&[0x80]).expect("obs-text is representable in a header value"),
        );
        assert_eq!(
            RequestContext::from_headers(&non_ascii, LocalSamplingDecision::Drop).incoming(),
            IncomingTrace::Rejected(TraceparentRejection::NonAscii)
        );
    }

    #[test]
    fn tracestate_multiple_fields_are_combined_validated_and_summarized() {
        let mut headers = headers_with_traceparent(&traceparent("00", "00"));
        headers.append(
            TRACESTATE_HEADER,
            HeaderValue::from_static("rojo=00f067aa0ba902b7"),
        );
        headers.append(
            TRACESTATE_HEADER,
            HeaderValue::from_static("congo= leading-space-is-data "),
        );
        let context = RequestContext::from_headers(&headers, LocalSamplingDecision::Drop);
        let link = context.external_link().expect("traceparent remains valid");
        assert_eq!(
            link.tracestate(),
            TracestateStatus::Valid { entry_count: 2 }
        );
    }

    #[test]
    fn invalid_tracestate_does_not_invalidate_traceparent_link() {
        let cases = [
            (
                "vendor=one,vendor=two".to_string(),
                TracestateRejection::DuplicateKey,
            ),
            ("Vendor=one".to_string(), TracestateRejection::InvalidKey),
            ("vendor=".to_string(), TracestateRejection::InvalidValue),
            (
                "vendor=one=two".to_string(),
                TracestateRejection::InvalidValue,
            ),
            (
                "missing-value".to_string(),
                TracestateRejection::MissingEquals,
            ),
            (
                (0..33)
                    .map(|index| format!("v{index}=x"))
                    .collect::<Vec<_>>()
                    .join(","),
                TracestateRejection::TooManyMembers,
            ),
            (
                format!("vendor={}", "x".repeat(MAX_TRACESTATE_VALUE_BYTES + 1)),
                TracestateRejection::InvalidValue,
            ),
        ];

        for (value, reason) in cases {
            let mut headers = headers_with_traceparent(&traceparent("00", "00"));
            headers.insert(
                TRACESTATE_HEADER,
                HeaderValue::from_str(&value).expect("test tracestate must be a header value"),
            );
            let context = RequestContext::from_headers(&headers, LocalSamplingDecision::Drop);
            let link = context
                .external_link()
                .expect("valid traceparent must remain linked");
            assert_eq!(
                link.tracestate(),
                TracestateStatus::Rejected(reason),
                "{value}"
            );
        }
    }

    #[test]
    fn overlong_tracestate_is_bounded_before_parsing() {
        let mut headers = headers_with_traceparent(&traceparent("00", "00"));
        let value = format!("vendor={}", "x".repeat(MAX_TRACESTATE_BYTES));
        headers.insert(
            TRACESTATE_HEADER,
            HeaderValue::from_str(&value).expect("ASCII test value"),
        );
        let context = RequestContext::from_headers(&headers, LocalSamplingDecision::Drop);
        assert_eq!(
            context.external_link().map(|link| link.tracestate()),
            Some(TracestateStatus::Rejected(TracestateRejection::TooLong))
        );
    }

    #[test]
    fn rejection_reasons_are_closed_safe_labels() {
        assert_eq!(TraceparentRejection::ZeroTraceId.as_str(), "zero_trace_id");
        assert_eq!(TracestateRejection::DuplicateKey.as_str(), "duplicate_key");
        assert_eq!(
            IncomingTrace::OrphanTracestate.as_str(),
            "orphan_tracestate"
        );
    }
}
