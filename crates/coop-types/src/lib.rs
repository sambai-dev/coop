use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

pub const MAX_OUTPUT_LINES: usize = 10_000;

pub const SUPPORTED_LANGUAGES: [&str; 3] = ["python", "node", "bash"];

pub const WALL_MAX_SECONDS: u32 = 300;
pub const CPU_MAX_SECONDS: u32 = 240;
pub const MEM_MAX_MB: u32 = 4096;
pub const PIDS_MAX: u32 = 1024;
pub const FILE_MAX_MB: u32 = 512;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct Limits {
    pub wall_seconds: u32,
    pub cpu_seconds: u32,
    pub mem_mb: u32,
    pub max_pids: u32,
    pub max_file_mb: u32,
    pub allow_network: bool,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            wall_seconds: 15,
            cpu_seconds: 10,
            mem_mb: 256,
            max_pids: 128,
            max_file_mb: 16,
            allow_network: false,
        }
    }
}

impl Limits {
    pub fn clamped(self) -> Self {
        Self {
            wall_seconds: self.wall_seconds.clamp(1, WALL_MAX_SECONDS),
            cpu_seconds: self.cpu_seconds.clamp(1, CPU_MAX_SECONDS),
            mem_mb: self.mem_mb.clamp(16, MEM_MAX_MB),
            max_pids: self.max_pids.clamp(8, PIDS_MAX),
            max_file_mb: self.max_file_mb.clamp(1, FILE_MAX_MB),
            allow_network: self.allow_network && false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct JobSpec {
    pub language: String,
    pub code: String,
    #[serde(default)]
    pub stdin: Option<String>,
    #[serde(default)]
    pub limits: Limits,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    TimedOut,
    OomKilled,
    Cancelled,
    Error,
}

impl JobStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            JobStatus::Queued => "queued",
            JobStatus::Running => "running",
            JobStatus::Succeeded => "succeeded",
            JobStatus::Failed => "failed",
            JobStatus::TimedOut => "timed_out",
            JobStatus::OomKilled => "oom_killed",
            JobStatus::Cancelled => "cancelled",
            JobStatus::Error => "error",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "queued" => JobStatus::Queued,
            "running" => JobStatus::Running,
            "succeeded" => JobStatus::Succeeded,
            "failed" => JobStatus::Failed,
            "timed_out" => JobStatus::TimedOut,
            "oom_killed" => JobStatus::OomKilled,
            "cancelled" => JobStatus::Cancelled,
            "error" => JobStatus::Error,
            _ => return None,
        })
    }

    pub fn is_terminal(self) -> bool {
        !matches!(self, JobStatus::Queued | JobStatus::Running)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutcomeStatus {
    Succeeded,
    Failed,
    TimedOut,
    OomKilled,
    Cancelled,
}

impl From<OutcomeStatus> for JobStatus {
    fn from(o: OutcomeStatus) -> Self {
        match o {
            OutcomeStatus::Succeeded => JobStatus::Succeeded,
            OutcomeStatus::Failed => JobStatus::Failed,
            OutcomeStatus::TimedOut => JobStatus::TimedOut,
            OutcomeStatus::OomKilled => JobStatus::OomKilled,
            OutcomeStatus::Cancelled => JobStatus::Cancelled,
        }
    }
}

pub fn is_supported_language(language: &str) -> bool {
    SUPPORTED_LANGUAGES.contains(&language)
}
