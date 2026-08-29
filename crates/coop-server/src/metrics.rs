//! Bounded, process-local operator telemetry.
//!
//! Metric dimensions are represented by closed enums rather than caller-owned
//! strings. That makes cardinality a compile-time property and prevents job,
//! tenant, request, trace, URL, and error-message data from entering labels.

use axum::http::HeaderValue;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const HTTP_DURATION_BUCKETS: [f64; 15] = [
    0.005, 0.01, 0.025, 0.05, 0.075, 0.1, 0.25, 0.5, 0.75, 1.0, 2.5, 5.0, 7.5, 10.0, 30.0,
];
const JOB_DURATION_BUCKETS: [f64; 14] = [
    0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0,
];
const STORAGE_DURATION_BUCKETS: [f64; 12] = [
    0.0005, 0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 5.0,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpositionFormat {
    Prometheus004,
    OpenMetrics100,
}

impl ExpositionFormat {
    pub const fn content_type(self) -> &'static str {
        match self {
            Self::Prometheus004 => "text/plain; version=0.0.4; charset=utf-8",
            Self::OpenMetrics100 => "application/openmetrics-text; version=1.0.0; charset=utf-8",
        }
    }
}

/// Select the best supported scrape protocol without reflecting any portion of
/// the untrusted Accept header. Invalid media ranges are ignored; the legacy
/// Prometheus text protocol is the compatibility fallback.
pub fn negotiate(accept: Option<&HeaderValue>) -> ExpositionFormat {
    let Some(raw) = accept.and_then(|value| value.to_str().ok()) else {
        return ExpositionFormat::Prometheus004;
    };
    let mut openmetrics_quality = -1.0_f32;
    let mut prometheus_quality = -1.0_f32;
    for range in raw.split(',').take(32) {
        let mut parts = range.split(';');
        let media_type = parts.next().unwrap_or_default().trim();
        let mut quality = 1.0_f32;
        let mut version = None;
        for parameter in parts.take(16) {
            let Some((name, value)) = parameter.trim().split_once('=') else {
                continue;
            };
            let value = value.trim().trim_matches('"');
            if name.trim().eq_ignore_ascii_case("q") {
                quality = value
                    .parse::<f32>()
                    .ok()
                    .filter(|q| (0.0..=1.0).contains(q))
                    .unwrap_or(0.0);
            } else if name.trim().eq_ignore_ascii_case("version") {
                version = Some(value);
            }
        }
        if quality <= 0.0 {
            continue;
        }
        if media_type.eq_ignore_ascii_case("application/openmetrics-text")
            && version.is_none_or(|value| value == "1.0.0")
        {
            openmetrics_quality = openmetrics_quality.max(quality);
        } else if (media_type.eq_ignore_ascii_case("text/plain")
            && version.is_none_or(|value| value == "0.0.4"))
            || media_type == "*/*"
        {
            prometheus_quality = prometheus_quality.max(quality);
        }
    }
    if openmetrics_quality >= 0.0 && openmetrics_quality >= prometheus_quality {
        ExpositionFormat::OpenMetrics100
    } else {
        ExpositionFormat::Prometheus004
    }
}

pub fn token_digest(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

pub fn token_matches(expected: &[u8; 32], presented: &str) -> bool {
    let presented = token_digest(presented);
    expected
        .iter()
        .zip(presented)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum HttpMethod {
    Get,
    Post,
    Delete,
    Other,
}

impl HttpMethod {
    pub fn classify(value: &axum::http::Method) -> Self {
        match *value {
            axum::http::Method::GET => Self::Get,
            axum::http::Method::POST => Self::Post,
            axum::http::Method::DELETE => Self::Delete,
            _ => Self::Other,
        }
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Delete => "DELETE",
            Self::Other => "OTHER",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum HttpRoute {
    Dashboard,
    Health,
    Ready,
    OpenApi,
    Metrics,
    Jobs,
    Job,
    Replay,
    Result,
    Stream,
    StreamTicket,
    Status,
    Capabilities,
    WhoAmI,
    WhoAmILegacy,
    Unmatched,
}

impl HttpRoute {
    pub fn classify(matched_path: Option<&str>) -> Self {
        match matched_path {
            Some("/") => Self::Dashboard,
            Some("/healthz") => Self::Health,
            Some("/readyz") => Self::Ready,
            Some("/openapi.json") => Self::OpenApi,
            Some("/metrics") => Self::Metrics,
            Some("/v1/jobs") => Self::Jobs,
            Some("/v1/jobs/{id}") => Self::Job,
            Some("/v1/jobs/{id}/replay") => Self::Replay,
            Some("/v1/jobs/{id}/result") => Self::Result,
            Some("/v1/jobs/{id}/stream") => Self::Stream,
            Some("/v1/jobs/{id}/stream-ticket") => Self::StreamTicket,
            Some("/v1/status") => Self::Status,
            Some("/v1/capabilities") => Self::Capabilities,
            Some("/v1/whoami") => Self::WhoAmI,
            Some("/whoami") => Self::WhoAmILegacy,
            _ => Self::Unmatched,
        }
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Dashboard => "/",
            Self::Health => "/healthz",
            Self::Ready => "/readyz",
            Self::OpenApi => "/openapi.json",
            Self::Metrics => "/metrics",
            Self::Jobs => "/v1/jobs",
            Self::Job => "/v1/jobs/{id}",
            Self::Replay => "/v1/jobs/{id}/replay",
            Self::Result => "/v1/jobs/{id}/result",
            Self::Stream => "/v1/jobs/{id}/stream",
            Self::StreamTicket => "/v1/jobs/{id}/stream-ticket",
            Self::Status => "/v1/status",
            Self::Capabilities => "/v1/capabilities",
            Self::WhoAmI => "/v1/whoami",
            Self::WhoAmILegacy => "/whoami",
            Self::Unmatched => "unmatched",
        }
    }
}

const ROUTE_METHODS: &[(HttpRoute, HttpMethod)] = &[
    (HttpRoute::Dashboard, HttpMethod::Get),
    (HttpRoute::Health, HttpMethod::Get),
    (HttpRoute::Ready, HttpMethod::Get),
    (HttpRoute::OpenApi, HttpMethod::Get),
    (HttpRoute::Metrics, HttpMethod::Get),
    (HttpRoute::Jobs, HttpMethod::Get),
    (HttpRoute::Jobs, HttpMethod::Post),
    (HttpRoute::Job, HttpMethod::Get),
    (HttpRoute::Job, HttpMethod::Delete),
    (HttpRoute::Replay, HttpMethod::Get),
    (HttpRoute::Result, HttpMethod::Get),
    (HttpRoute::Stream, HttpMethod::Get),
    (HttpRoute::StreamTicket, HttpMethod::Post),
    (HttpRoute::Status, HttpMethod::Get),
    (HttpRoute::Capabilities, HttpMethod::Get),
    (HttpRoute::WhoAmI, HttpMethod::Get),
    (HttpRoute::WhoAmILegacy, HttpMethod::Get),
    (HttpRoute::Unmatched, HttpMethod::Other),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum StatusClass {
    Informational,
    Success,
    Redirection,
    ClientError,
    ServerError,
    Other,
}

impl StatusClass {
    const ALL: [Self; 6] = [
        Self::Informational,
        Self::Success,
        Self::Redirection,
        Self::ClientError,
        Self::ServerError,
        Self::Other,
    ];

    fn from_code(status: u16) -> Self {
        match status {
            100..=199 => Self::Informational,
            200..=299 => Self::Success,
            300..=399 => Self::Redirection,
            400..=499 => Self::ClientError,
            500..=599 => Self::ServerError,
            _ => Self::Other,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Informational => "1xx",
            Self::Success => "2xx",
            Self::Redirection => "3xx",
            Self::ClientError => "4xx",
            Self::ServerError => "5xx",
            Self::Other => "other",
        }
    }
}

#[derive(Clone)]
struct Histogram<const N: usize> {
    buckets: [u64; N],
    count: u64,
    sum: f64,
}

impl<const N: usize> Default for Histogram<N> {
    fn default() -> Self {
        Self {
            buckets: [0; N],
            count: 0,
            sum: 0.0,
        }
    }
}

impl<const N: usize> Histogram<N> {
    fn observe(&mut self, value: f64, boundaries: &[f64; N]) {
        let value = if value.is_finite() && value >= 0.0 {
            value
        } else {
            0.0
        };
        for (count, boundary) in self.buckets.iter_mut().zip(boundaries) {
            if value <= *boundary {
                *count = count.saturating_add(1);
            }
        }
        self.count = self.count.saturating_add(1);
        self.sum += value;
    }
}

#[derive(Clone, Default)]
struct HttpState {
    requests: BTreeMap<(HttpRoute, HttpMethod, StatusClass), u64>,
    duration: BTreeMap<(HttpRoute, HttpMethod), Histogram<15>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(usize)]
pub enum Language {
    Python = 0,
    Node = 1,
    Bash = 2,
    Other = 3,
}

impl Language {
    pub fn classify(value: &str) -> Self {
        match value {
            "python" => Self::Python,
            "node" => Self::Node,
            "bash" => Self::Bash,
            _ => Self::Other,
        }
    }

    const ALL: [Self; 4] = [Self::Python, Self::Node, Self::Bash, Self::Other];

    const fn label(self) -> &'static str {
        match self {
            Self::Python => "python",
            Self::Node => "node",
            Self::Bash => "bash",
            Self::Other => "other",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(usize)]
pub enum JobOutcome {
    Succeeded = 0,
    Failed = 1,
    TimedOut = 2,
    OomKilled = 3,
    Cancelled = 4,
    Error = 5,
    Aborted = 6,
}

impl JobOutcome {
    pub fn classify(value: &str) -> Self {
        match value {
            "succeeded" => Self::Succeeded,
            "failed" => Self::Failed,
            "timed_out" => Self::TimedOut,
            "oom_killed" => Self::OomKilled,
            "cancelled" => Self::Cancelled,
            "error" => Self::Error,
            _ => Self::Aborted,
        }
    }

    const ALL: [Self; 7] = [
        Self::Succeeded,
        Self::Failed,
        Self::TimedOut,
        Self::OomKilled,
        Self::Cancelled,
        Self::Error,
        Self::Aborted,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::TimedOut => "timed_out",
            Self::OomKilled => "oom_killed",
            Self::Cancelled => "cancelled",
            Self::Error => "error",
            Self::Aborted => "aborted",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum AdmissionScope {
    Queue = 0,
    Scheduler = 1,
    SubmitBody = 2,
    Stream = 3,
    ResultWait = 4,
    LargeResponse = 5,
    Rate = 6,
}

impl AdmissionScope {
    const ALL: [Self; 7] = [
        Self::Queue,
        Self::Scheduler,
        Self::SubmitBody,
        Self::Stream,
        Self::ResultWait,
        Self::LargeResponse,
        Self::Rate,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Queue => "queue",
            Self::Scheduler => "scheduler",
            Self::SubmitBody => "submit_body",
            Self::Stream => "stream",
            Self::ResultWait => "result_wait",
            Self::LargeResponse => "large_response",
            Self::Rate => "rate",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum AdmissionReason {
    GlobalFull = 0,
    TenantFull = 1,
    Closed = 2,
    RateLimited = 3,
    Startup = 4,
    Shutdown = 5,
}

impl AdmissionReason {
    const ALL: [Self; 6] = [
        Self::GlobalFull,
        Self::TenantFull,
        Self::Closed,
        Self::RateLimited,
        Self::Startup,
        Self::Shutdown,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::GlobalFull => "global_full",
            Self::TenantFull => "tenant_full",
            Self::Closed => "closed",
            Self::RateLimited => "rate_limited",
            Self::Startup => "startup",
            Self::Shutdown => "shutdown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(usize)]
pub enum StorageOperation {
    Read = 0,
    Accept = 1,
    Start = 2,
    Events = 3,
    Finalize = 4,
    Recovery = 5,
    Retention = 6,
    Readiness = 7,
}

impl StorageOperation {
    const ALL: [Self; 8] = [
        Self::Read,
        Self::Accept,
        Self::Start,
        Self::Events,
        Self::Finalize,
        Self::Recovery,
        Self::Retention,
        Self::Readiness,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Accept => "accept",
            Self::Start => "start",
            Self::Events => "events",
            Self::Finalize => "finalize",
            Self::Recovery => "recovery",
            Self::Retention => "retention",
            Self::Readiness => "readiness",
        }
    }
}

#[derive(Clone, Default)]
struct StorageStats {
    attempts: u64,
    errors: u64,
    duration: Histogram<12>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum TruncationKind {
    Stdout = 0,
    Stderr = 1,
    Evidence = 2,
}

impl TruncationKind {
    const ALL: [Self; 3] = [Self::Stdout, Self::Stderr, Self::Evidence];

    const fn label(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
            Self::Evidence => "evidence",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum RecoveryKind {
    InterruptedRunning = 0,
    RestoredQueued = 1,
}

impl RecoveryKind {
    const ALL: [Self; 2] = [Self::InterruptedRunning, Self::RestoredQueued];

    const fn label(self) -> &'static str {
        match self {
            Self::InterruptedRunning => "interrupted_running",
            Self::RestoredQueued => "restored_queued",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CapacitySnapshot {
    pub queue_used: usize,
    pub queue_limit: usize,
    pub submit_bodies_used: usize,
    pub submit_bodies_limit: usize,
    pub streams_used: usize,
    pub streams_limit: usize,
    pub result_waits_used: usize,
    pub result_waits_limit: usize,
    pub large_responses_used: usize,
    pub large_responses_limit: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct ReadinessSnapshot {
    pub ready: bool,
    pub startup: bool,
    pub storage: bool,
    pub scheduler: bool,
    pub accepting: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct RuntimeSnapshot {
    pub capacity: CapacitySnapshot,
    pub readiness: ReadinessSnapshot,
}

pub struct Metrics {
    started_at: Instant,
    http_active: AtomicU64,
    http: Mutex<HttpState>,
    jobs_submitted: [AtomicU64; 4],
    jobs_completed: Vec<AtomicU64>,
    job_duration: Mutex<BTreeMap<(Language, JobOutcome), Histogram<14>>>,
    execution_active: AtomicU64,
    admission_rejections: Vec<AtomicU64>,
    storage: Mutex<BTreeMap<StorageOperation, StorageStats>>,
    truncations: [AtomicU64; 3],
    recoveries: [AtomicU64; 2],
    retention_runs: AtomicU64,
    retention_errors: AtomicU64,
    retention_jobs_deleted: AtomicU64,
    retention_events_deleted: AtomicU64,
    retention_last_success_seconds: AtomicU64,
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

impl Metrics {
    pub fn new() -> Self {
        let mut http = HttpState::default();
        for &(route, method) in ROUTE_METHODS {
            http.duration.insert((route, method), Histogram::default());
            for status in StatusClass::ALL {
                http.requests.insert((route, method, status), 0);
            }
        }
        let mut job_duration = BTreeMap::new();
        for language in Language::ALL {
            for outcome in JobOutcome::ALL {
                job_duration.insert((language, outcome), Histogram::default());
            }
        }
        let mut storage = BTreeMap::new();
        for operation in StorageOperation::ALL {
            storage.insert(operation, StorageStats::default());
        }
        Self {
            started_at: Instant::now(),
            http_active: AtomicU64::new(0),
            http: Mutex::new(http),
            jobs_submitted: std::array::from_fn(|_| AtomicU64::new(0)),
            jobs_completed: (0..Language::ALL.len() * JobOutcome::ALL.len())
                .map(|_| AtomicU64::new(0))
                .collect(),
            job_duration: Mutex::new(job_duration),
            execution_active: AtomicU64::new(0),
            admission_rejections: (0..AdmissionScope::ALL.len() * AdmissionReason::ALL.len())
                .map(|_| AtomicU64::new(0))
                .collect(),
            storage: Mutex::new(storage),
            truncations: std::array::from_fn(|_| AtomicU64::new(0)),
            recoveries: std::array::from_fn(|_| AtomicU64::new(0)),
            retention_runs: AtomicU64::new(0),
            retention_errors: AtomicU64::new(0),
            retention_jobs_deleted: AtomicU64::new(0),
            retention_events_deleted: AtomicU64::new(0),
            retention_last_success_seconds: AtomicU64::new(0),
        }
    }

    pub fn start_http(
        self: &std::sync::Arc<Self>,
        method: HttpMethod,
        route: HttpRoute,
    ) -> HttpObservation {
        self.http_active.fetch_add(1, Ordering::Relaxed);
        HttpObservation {
            metrics: std::sync::Arc::clone(self),
            method,
            route,
            started_at: Instant::now(),
            finished: false,
        }
    }

    pub fn submitted(&self, language: Language) {
        self.jobs_submitted[language as usize].fetch_add(1, Ordering::Relaxed);
    }

    pub fn start_execution(
        self: &std::sync::Arc<Self>,
        language: Language,
    ) -> ExecutionObservation {
        self.execution_active.fetch_add(1, Ordering::Relaxed);
        ExecutionObservation {
            metrics: std::sync::Arc::clone(self),
            language,
            started_at: Instant::now(),
            finished: false,
        }
    }

    pub fn reject(&self, scope: AdmissionScope, reason: AdmissionReason) {
        let index = scope as usize * AdmissionReason::ALL.len() + reason as usize;
        self.admission_rejections[index].fetch_add(1, Ordering::Relaxed);
    }

    pub fn observe_storage(&self, operation: StorageOperation, duration: Duration, ok: bool) {
        let mut storage = self
            .storage
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let stats = storage
            .get_mut(&operation)
            .expect("all storage operations are pre-registered");
        stats.attempts = stats.attempts.saturating_add(1);
        if !ok {
            stats.errors = stats.errors.saturating_add(1);
        }
        stats
            .duration
            .observe(duration.as_secs_f64(), &STORAGE_DURATION_BUCKETS);
    }

    pub fn truncation(&self, kind: TruncationKind) {
        self.truncations[kind as usize].fetch_add(1, Ordering::Relaxed);
    }

    pub fn recovered(&self, kind: RecoveryKind, count: u64) {
        self.recoveries[kind as usize].fetch_add(count, Ordering::Relaxed);
    }

    pub fn retention_succeeded(&self, jobs_deleted: u64, events_deleted: u64) {
        self.retention_runs.fetch_add(1, Ordering::Relaxed);
        self.retention_jobs_deleted
            .fetch_add(jobs_deleted, Ordering::Relaxed);
        self.retention_events_deleted
            .fetch_add(events_deleted, Ordering::Relaxed);
        self.retention_last_success_seconds
            .store(unix_seconds(), Ordering::Relaxed);
    }

    pub fn retention_failed(&self) {
        self.retention_runs.fetch_add(1, Ordering::Relaxed);
        self.retention_errors.fetch_add(1, Ordering::Relaxed);
    }

    pub fn render(&self, format: ExpositionFormat, snapshot: RuntimeSnapshot) -> String {
        let mut output = String::with_capacity(48 * 1024);
        metric_header(
            &mut output,
            format,
            "coop_up",
            "Whether this Coop process is running.",
            "gauge",
            None,
        );
        writeln!(output, "coop_up 1").expect("write string");
        metric_header(
            &mut output,
            format,
            "coop_process_uptime_seconds",
            "Seconds since this Coop process initialized telemetry.",
            "gauge",
            Some("seconds"),
        );
        writeln!(
            output,
            "coop_process_uptime_seconds {}",
            self.started_at.elapsed().as_secs()
        )
        .expect("write string");
        metric_header(
            &mut output,
            format,
            "coop_build_info",
            "Build information for the running Coop binary.",
            "gauge",
            None,
        );
        let version = escape_label_value(env!("CARGO_PKG_VERSION"));
        let revision = escape_label_value(option_env!("COOP_GIT_REVISION").unwrap_or("unknown"));
        writeln!(
            output,
            "coop_build_info{{version=\"{}\",revision=\"{}\"}} 1",
            version, revision
        )
        .expect("write string");

        self.render_http(format, &mut output);
        self.render_jobs(format, &mut output);
        self.render_admission(format, &mut output);
        self.render_storage(format, &mut output);
        self.render_recovery_retention(format, &mut output);
        render_runtime(format, &mut output, snapshot);

        if matches!(format, ExpositionFormat::OpenMetrics100) {
            output.push_str("# EOF\n");
        }
        output
    }

    fn render_http(&self, format: ExpositionFormat, output: &mut String) {
        let http = self
            .http
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        metric_header(
            output,
            format,
            "coop_http_server_active_requests",
            "HTTP requests currently executing in Coop.",
            "gauge",
            None,
        );
        writeln!(
            output,
            "coop_http_server_active_requests {}",
            self.http_active.load(Ordering::Relaxed)
        )
        .expect("write string");
        metric_header(
            output,
            format,
            "coop_http_server_requests_total",
            "Completed HTTP requests by bounded method, matched route, and status class.",
            "counter",
            None,
        );
        for (&(route, method, status), count) in &http.requests {
            writeln!(output, "coop_http_server_requests_total{{method=\"{}\",route=\"{}\",status_class=\"{}\"}} {}", method.label(), route.label(), status.label(), count).expect("write string");
        }
        metric_header(
            output,
            format,
            "coop_http_server_request_duration_seconds",
            "HTTP server request duration in seconds.",
            "histogram",
            Some("seconds"),
        );
        for (&(route, method), histogram) in &http.duration {
            render_histogram(
                output,
                "coop_http_server_request_duration_seconds",
                &[("method", method.label()), ("route", route.label())],
                histogram,
                &HTTP_DURATION_BUCKETS,
            );
        }
    }

    fn render_jobs(&self, format: ExpositionFormat, output: &mut String) {
        metric_header(
            output,
            format,
            "coop_jobs_submitted_total",
            "Durably accepted jobs by language.",
            "counter",
            None,
        );
        for language in Language::ALL {
            writeln!(
                output,
                "coop_jobs_submitted_total{{language=\"{}\"}} {}",
                language.label(),
                self.jobs_submitted[language as usize].load(Ordering::Relaxed)
            )
            .expect("write string");
        }
        metric_header(
            output,
            format,
            "coop_jobs_completed_total",
            "Completed job executions by language and terminal outcome.",
            "counter",
            None,
        );
        for language in Language::ALL {
            for outcome in JobOutcome::ALL {
                let index = language as usize * JobOutcome::ALL.len() + outcome as usize;
                writeln!(
                    output,
                    "coop_jobs_completed_total{{language=\"{}\",status=\"{}\"}} {}",
                    language.label(),
                    outcome.label(),
                    self.jobs_completed[index].load(Ordering::Relaxed)
                )
                .expect("write string");
            }
        }
        metric_header(
            output,
            format,
            "coop_executions_active",
            "Workloads currently inside an executor backend.",
            "gauge",
            None,
        );
        writeln!(
            output,
            "coop_executions_active {}",
            self.execution_active.load(Ordering::Relaxed)
        )
        .expect("write string");
        metric_header(
            output,
            format,
            "coop_job_execution_duration_seconds",
            "Executor workload duration in seconds by language and terminal outcome.",
            "histogram",
            Some("seconds"),
        );
        let duration = self
            .job_duration
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        for (&(language, outcome), histogram) in duration.iter() {
            render_histogram(
                output,
                "coop_job_execution_duration_seconds",
                &[("language", language.label()), ("status", outcome.label())],
                histogram,
                &JOB_DURATION_BUCKETS,
            );
        }
    }

    fn render_admission(&self, format: ExpositionFormat, output: &mut String) {
        metric_header(
            output,
            format,
            "coop_admission_rejections_total",
            "Rejected work by bounded admission scope and reason.",
            "counter",
            None,
        );
        for scope in AdmissionScope::ALL {
            for reason in AdmissionReason::ALL {
                let index = scope as usize * AdmissionReason::ALL.len() + reason as usize;
                writeln!(
                    output,
                    "coop_admission_rejections_total{{scope=\"{}\",reason=\"{}\"}} {}",
                    scope.label(),
                    reason.label(),
                    self.admission_rejections[index].load(Ordering::Relaxed)
                )
                .expect("write string");
            }
        }
    }

    fn render_storage(&self, format: ExpositionFormat, output: &mut String) {
        let storage = self
            .storage
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        metric_header(
            output,
            format,
            "coop_storage_operations_total",
            "SQLite operations attempted by bounded lifecycle stage.",
            "counter",
            None,
        );
        for operation in StorageOperation::ALL {
            let stats = storage.get(&operation).expect("storage stat exists");
            writeln!(
                output,
                "coop_storage_operations_total{{operation=\"{}\"}} {}",
                operation.label(),
                stats.attempts
            )
            .expect("write string");
        }
        metric_header(
            output,
            format,
            "coop_storage_errors_total",
            "SQLite operation failures by bounded lifecycle stage.",
            "counter",
            None,
        );
        for operation in StorageOperation::ALL {
            let stats = storage.get(&operation).expect("storage stat exists");
            writeln!(
                output,
                "coop_storage_errors_total{{operation=\"{}\"}} {}",
                operation.label(),
                stats.errors
            )
            .expect("write string");
        }
        metric_header(
            output,
            format,
            "coop_storage_operation_duration_seconds",
            "SQLite operation duration in seconds by bounded lifecycle stage.",
            "histogram",
            Some("seconds"),
        );
        for operation in StorageOperation::ALL {
            let stats = storage.get(&operation).expect("storage stat exists");
            render_histogram(
                output,
                "coop_storage_operation_duration_seconds",
                &[("operation", operation.label())],
                &stats.duration,
                &STORAGE_DURATION_BUCKETS,
            );
        }
    }

    fn render_recovery_retention(&self, format: ExpositionFormat, output: &mut String) {
        metric_header(
            output,
            format,
            "coop_output_truncations_total",
            "Output or evidence truncation signals by bounded kind.",
            "counter",
            None,
        );
        for kind in TruncationKind::ALL {
            writeln!(
                output,
                "coop_output_truncations_total{{kind=\"{}\"}} {}",
                kind.label(),
                self.truncations[kind as usize].load(Ordering::Relaxed)
            )
            .expect("write string");
        }
        metric_header(
            output,
            format,
            "coop_recovered_jobs_total",
            "Jobs reconciled during process startup by recovery kind.",
            "counter",
            None,
        );
        for kind in RecoveryKind::ALL {
            writeln!(
                output,
                "coop_recovered_jobs_total{{kind=\"{}\"}} {}",
                kind.label(),
                self.recoveries[kind as usize].load(Ordering::Relaxed)
            )
            .expect("write string");
        }
        metric_header(
            output,
            format,
            "coop_retention_runs_total",
            "Retention sweeps attempted.",
            "counter",
            None,
        );
        writeln!(
            output,
            "coop_retention_runs_total {}",
            self.retention_runs.load(Ordering::Relaxed)
        )
        .expect("write string");
        metric_header(
            output,
            format,
            "coop_retention_errors_total",
            "Retention sweeps that encountered a storage error.",
            "counter",
            None,
        );
        writeln!(
            output,
            "coop_retention_errors_total {}",
            self.retention_errors.load(Ordering::Relaxed)
        )
        .expect("write string");
        metric_header(
            output,
            format,
            "coop_retention_jobs_deleted_total",
            "Terminal jobs deleted by retention.",
            "counter",
            None,
        );
        writeln!(
            output,
            "coop_retention_jobs_deleted_total {}",
            self.retention_jobs_deleted.load(Ordering::Relaxed)
        )
        .expect("write string");
        metric_header(
            output,
            format,
            "coop_retention_events_deleted_total",
            "Job events deleted by retention.",
            "counter",
            None,
        );
        writeln!(
            output,
            "coop_retention_events_deleted_total {}",
            self.retention_events_deleted.load(Ordering::Relaxed)
        )
        .expect("write string");
        metric_header(
            output,
            format,
            "coop_retention_last_success_timestamp_seconds",
            "Unix timestamp of the last successful retention sweep, or zero before one succeeds.",
            "gauge",
            Some("seconds"),
        );
        writeln!(
            output,
            "coop_retention_last_success_timestamp_seconds {}",
            self.retention_last_success_seconds.load(Ordering::Relaxed)
        )
        .expect("write string");
    }
}

pub struct HttpObservation {
    metrics: std::sync::Arc<Metrics>,
    method: HttpMethod,
    route: HttpRoute,
    started_at: Instant,
    finished: bool,
}

impl HttpObservation {
    pub fn finish(mut self, status: u16) {
        self.metrics
            .observe_http(self.method, self.route, status, self.started_at.elapsed());
        self.finished = true;
    }
}

impl Drop for HttpObservation {
    fn drop(&mut self) {
        self.metrics.http_active.fetch_sub(1, Ordering::Relaxed);
        if !self.finished {
            self.metrics
                .observe_http(self.method, self.route, 500, self.started_at.elapsed());
        }
    }
}

impl Metrics {
    fn observe_http(&self, method: HttpMethod, route: HttpRoute, status: u16, duration: Duration) {
        let mut http = self
            .http
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let count = http
            .requests
            .entry((route, method, StatusClass::from_code(status)))
            .or_default();
        *count = count.saturating_add(1);
        http.duration
            .entry((route, method))
            .or_default()
            .observe(duration.as_secs_f64(), &HTTP_DURATION_BUCKETS);
    }
}

pub struct ExecutionObservation {
    metrics: std::sync::Arc<Metrics>,
    language: Language,
    started_at: Instant,
    finished: bool,
}

impl ExecutionObservation {
    pub fn finish(mut self, outcome: JobOutcome) {
        self.metrics
            .finish_execution(self.language, outcome, self.started_at.elapsed());
        self.finished = true;
    }
}

impl Drop for ExecutionObservation {
    fn drop(&mut self) {
        self.metrics
            .execution_active
            .fetch_sub(1, Ordering::Relaxed);
        if !self.finished {
            self.metrics.finish_execution(
                self.language,
                JobOutcome::Aborted,
                self.started_at.elapsed(),
            );
        }
    }
}

impl Metrics {
    fn finish_execution(&self, language: Language, outcome: JobOutcome, duration: Duration) {
        let index = language as usize * JobOutcome::ALL.len() + outcome as usize;
        self.jobs_completed[index].fetch_add(1, Ordering::Relaxed);
        self.job_duration
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_mut(&(language, outcome))
            .expect("job histogram exists")
            .observe(duration.as_secs_f64(), &JOB_DURATION_BUCKETS);
    }
}

fn render_runtime(format: ExpositionFormat, output: &mut String, snapshot: RuntimeSnapshot) {
    metric_header(
        output,
        format,
        "coop_ready",
        "Whether Coop is ready to accept production traffic.",
        "gauge",
        None,
    );
    writeln!(output, "coop_ready {}", u8::from(snapshot.readiness.ready)).expect("write string");
    metric_header(
        output,
        format,
        "coop_readiness_component",
        "Readiness component state; one means healthy or accepting.",
        "gauge",
        None,
    );
    for (component, ready) in [
        ("startup", snapshot.readiness.startup),
        ("storage", snapshot.readiness.storage),
        ("scheduler", snapshot.readiness.scheduler),
        ("accepting", snapshot.readiness.accepting),
    ] {
        writeln!(
            output,
            "coop_readiness_component{{component=\"{component}\"}} {}",
            u8::from(ready)
        )
        .expect("write string");
    }
    metric_header(
        output,
        format,
        "coop_queue_depth",
        "Process-local accepted jobs retaining a queue lease.",
        "gauge",
        None,
    );
    writeln!(output, "coop_queue_depth {}", snapshot.capacity.queue_used).expect("write string");
    metric_header(
        output,
        format,
        "coop_queue_capacity",
        "Maximum process-local accepted jobs retaining queue leases.",
        "gauge",
        None,
    );
    writeln!(
        output,
        "coop_queue_capacity {}",
        snapshot.capacity.queue_limit
    )
    .expect("write string");
    metric_header(
        output,
        format,
        "coop_capacity_used",
        "Currently used bounded server capacity by resource.",
        "gauge",
        None,
    );
    let capacities = [
        (
            "submit_bodies",
            snapshot.capacity.submit_bodies_used,
            snapshot.capacity.submit_bodies_limit,
        ),
        (
            "streams",
            snapshot.capacity.streams_used,
            snapshot.capacity.streams_limit,
        ),
        (
            "result_waits",
            snapshot.capacity.result_waits_used,
            snapshot.capacity.result_waits_limit,
        ),
        (
            "large_responses",
            snapshot.capacity.large_responses_used,
            snapshot.capacity.large_responses_limit,
        ),
    ];
    for (resource, used, _) in capacities {
        writeln!(
            output,
            "coop_capacity_used{{resource=\"{resource}\"}} {used}"
        )
        .expect("write string");
    }
    metric_header(
        output,
        format,
        "coop_capacity_limit",
        "Configured bounded server capacity by resource.",
        "gauge",
        None,
    );
    for (resource, _, limit) in capacities {
        writeln!(
            output,
            "coop_capacity_limit{{resource=\"{resource}\"}} {limit}"
        )
        .expect("write string");
    }
}

fn metric_header(
    output: &mut String,
    format: ExpositionFormat,
    name: &str,
    help: &str,
    metric_type: &str,
    unit: Option<&str>,
) {
    writeln!(output, "# HELP {name} {help}").expect("write string");
    writeln!(output, "# TYPE {name} {metric_type}").expect("write string");
    if matches!(format, ExpositionFormat::OpenMetrics100) {
        if let Some(unit) = unit {
            writeln!(output, "# UNIT {name} {unit}").expect("write string");
        }
    }
}

fn render_histogram<const N: usize>(
    output: &mut String,
    name: &str,
    labels: &[(&str, &str)],
    histogram: &Histogram<N>,
    boundaries: &[f64; N],
) {
    for (boundary, count) in boundaries.iter().zip(histogram.buckets) {
        write!(output, "{name}_bucket{{").expect("write string");
        render_labels(output, labels);
        writeln!(output, "le=\"{boundary}\"}} {count}").expect("write string");
    }
    write!(output, "{name}_bucket{{").expect("write string");
    render_labels(output, labels);
    writeln!(output, "le=\"+Inf\"}} {}", histogram.count).expect("write string");
    write!(output, "{name}_sum{{").expect("write string");
    render_labels_without_trailing_comma(output, labels);
    writeln!(output, "}} {}", histogram.sum).expect("write string");
    write!(output, "{name}_count{{").expect("write string");
    render_labels_without_trailing_comma(output, labels);
    writeln!(output, "}} {}", histogram.count).expect("write string");
}

fn render_labels(output: &mut String, labels: &[(&str, &str)]) {
    for (name, value) in labels {
        write!(output, "{name}=\"{value}\",").expect("write string");
    }
}

fn render_labels_without_trailing_comma(output: &mut String, labels: &[(&str, &str)]) {
    for (index, (name, value)) in labels.iter().enumerate() {
        if index > 0 {
            output.push(',');
        }
        write!(output, "{name}=\"{value}\"").expect("write string");
    }
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn escape_label_value(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' | '\r' => escaped.push_str("\\n"),
            character if character.is_control() => escaped.push('_'),
            character => escaped.push(character),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn snapshot() -> RuntimeSnapshot {
        RuntimeSnapshot {
            capacity: CapacitySnapshot {
                queue_used: 3,
                queue_limit: 1024,
                submit_bodies_used: 1,
                submit_bodies_limit: 4,
                streams_used: 2,
                streams_limit: 128,
                result_waits_used: 0,
                result_waits_limit: 64,
                large_responses_used: 0,
                large_responses_limit: 4,
            },
            readiness: ReadinessSnapshot {
                ready: true,
                startup: true,
                storage: true,
                scheduler: true,
                accepting: true,
            },
        }
    }

    #[test]
    fn negotiates_openmetrics_only_when_supported_and_acceptable() {
        assert_eq!(negotiate(None), ExpositionFormat::Prometheus004);
        let open = HeaderValue::from_static(
            "application/openmetrics-text;version=1.0.0;q=0.9,text/plain;version=0.0.4;q=0.2",
        );
        assert_eq!(negotiate(Some(&open)), ExpositionFormat::OpenMetrics100);
        let legacy = HeaderValue::from_static(
            "application/openmetrics-text;version=2.0.0;q=1,text/plain;version=0.0.4;q=0.8",
        );
        assert_eq!(negotiate(Some(&legacy)), ExpositionFormat::Prometheus004);
        let disabled = HeaderValue::from_static(
            "application/openmetrics-text;version=1.0.0;q=0,text/plain;q=0.1",
        );
        assert_eq!(negotiate(Some(&disabled)), ExpositionFormat::Prometheus004);
    }

    #[test]
    fn renders_valid_bounded_openmetrics_without_sensitive_dimensions() {
        let metrics = Arc::new(Metrics::new());
        metrics.submitted(Language::Python);
        metrics.reject(AdmissionScope::Queue, AdmissionReason::GlobalFull);
        metrics.observe_storage(StorageOperation::Accept, Duration::from_millis(2), true);
        metrics.truncation(TruncationKind::Stdout);
        metrics.recovered(RecoveryKind::RestoredQueued, 2);
        let observation = metrics.start_http(HttpMethod::Post, HttpRoute::Jobs);
        observation.finish(201);
        let execution = metrics.start_execution(Language::Python);
        execution.finish(JobOutcome::Succeeded);

        let body = metrics.render(ExpositionFormat::OpenMetrics100, snapshot());
        assert!(body.ends_with("# EOF\n"));
        assert!(body.contains("coop_jobs_submitted_total{language=\"python\"} 1"));
        assert!(body.contains("coop_executions_active 0"));
        assert!(body.contains("coop_queue_depth 3"));
        assert!(body.contains("coop_build_info"));
        for forbidden in ["tenant=", "job_id=", "request_id=", "trace_id=", "url="] {
            assert!(!body.contains(forbidden), "leaked label {forbidden}");
        }
    }

    #[test]
    fn dropped_observation_guards_reclaim_gauges_and_record_aborts() {
        let metrics = Arc::new(Metrics::new());
        drop(metrics.start_http(HttpMethod::Get, HttpRoute::Health));
        drop(metrics.start_execution(Language::Bash));
        let body = metrics.render(ExpositionFormat::Prometheus004, snapshot());
        assert!(body.contains("coop_http_server_active_requests 0"));
        assert!(body.contains("coop_executions_active 0"));
        assert!(body.contains("coop_jobs_completed_total{language=\"bash\",status=\"aborted\"} 1"));
        assert!(!body.contains("# EOF"));
    }

    #[test]
    fn route_classifier_never_uses_raw_dynamic_paths() {
        assert_eq!(
            HttpRoute::classify(Some("/v1/jobs/{id}/result")),
            HttpRoute::Result
        );
        assert_eq!(
            HttpRoute::classify(Some("/v1/jobs/secret-job/result")),
            HttpRoute::Unmatched
        );
        assert_eq!(HttpRoute::classify(None), HttpRoute::Unmatched);
    }

    #[test]
    fn scrape_token_comparison_is_digest_based_and_exact() {
        let expected = token_digest("operator-secret");
        assert!(token_matches(&expected, "operator-secret"));
        assert!(!token_matches(&expected, "operator-secreu"));
        assert!(!token_matches(&expected, "operator-secret-longer"));
    }

    #[test]
    fn dynamic_build_labels_are_exposition_escaped() {
        assert_eq!(
            escape_label_value("rev\"\\line\nnext\t"),
            "rev\\\"\\\\line\\nnext_"
        );
    }
}
