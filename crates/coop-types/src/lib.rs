use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

pub const MAX_OUTPUT_LINES: usize = 10_000;
/// Maximum payload bytes retained and emitted for each output stream.
/// Executors continue draining fixed-size chunks after this boundary so a
/// noisy child cannot block on a full pipe or allocate unbounded host memory.
pub const MAX_OUTPUT_BYTES_PER_STREAM: usize = 1024 * 1024;
/// Maximum bytes retained for a single logical output record. Longer records
/// are split deterministically and marked truncated by the executor.
pub const MAX_OUTPUT_RECORD_BYTES: usize = 16 * 1024;
pub const MAX_CODE_BYTES: usize = 1024 * 1024;
pub const MAX_STDIN_BYTES: usize = 1024 * 1024;

pub const SUPPORTED_LANGUAGES: [&str; 3] = ["python", "node", "bash"];

pub const WALL_MAX_SECONDS: u32 = 300;
pub const CPU_MAX_SECONDS: u32 = 240;
pub const MEM_MAX_MB: u32 = 4096;
pub const PIDS_MAX: u32 = 1024;
pub const FILE_MAX_MB: u32 = 512;

/// The runtime boundary a job requires or actually reached.
///
/// Most process-style providers form a monotonic chain from no isolation to
/// confidential hardware. `wasm-capability` is deliberately a separate
/// branch: a VM does not silently satisfy a caller that explicitly requested
/// a capability-oriented Wasm host interface, and vice versa.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "kebab-case")]
pub enum IsolationClass {
    #[default]
    None,
    LinuxSharedKernel,
    GvisorApplicationKernel,
    WasmCapability,
    HardwareVm,
    ConfidentialVm,
}

impl IsolationClass {
    /// Whether this observed provider class satisfies a requested minimum.
    pub const fn satisfies(self, minimum: Self) -> bool {
        use IsolationClass::*;
        match minimum {
            None => true,
            WasmCapability => matches!(self, WasmCapability),
            LinuxSharedKernel => matches!(
                self,
                LinuxSharedKernel | GvisorApplicationKernel | HardwareVm | ConfidentialVm
            ),
            GvisorApplicationKernel => {
                matches!(self, GvisorApplicationKernel | HardwareVm | ConfidentialVm)
            }
            HardwareVm => matches!(self, HardwareVm | ConfidentialVm),
            ConfidentialVm => matches!(self, ConfidentialVm),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default, deny_unknown_fields)]
pub struct JobRequirements {
    pub minimum_isolation: IsolationClass,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default, deny_unknown_fields)]
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
            allow_network: false,
        }
    }
}

/// Backend-specific limits that were actually effective for an execution.
///
/// A `None` control is deliberately different from the requested value: it
/// means that the selected executor did not enforce that control (or that the
/// executor never reached its ready boundary). `allow_network` is observed
/// runtime posture rather than a resource limit and is `None` until a
/// workload becomes ready.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EffectiveLimits {
    pub wall_seconds: Option<u32>,
    pub cpu_seconds: Option<u32>,
    pub mem_mb: Option<u32>,
    pub max_pids: Option<u32>,
    pub max_file_mb: Option<u32>,
    pub allow_network: Option<bool>,
}

impl EffectiveLimits {
    pub fn from_enforcement(
        limits: &Limits,
        enforcement: &LimitEnforcement,
        allow_network: Option<bool>,
    ) -> Self {
        Self {
            wall_seconds: enforcement.wall_seconds.then_some(limits.wall_seconds),
            cpu_seconds: enforcement.cpu_seconds.then_some(limits.cpu_seconds),
            mem_mb: enforcement.mem_mb.then_some(limits.mem_mb),
            max_pids: enforcement.max_pids.then_some(limits.max_pids),
            max_file_mb: enforcement.max_file_mb.then_some(limits.max_file_mb),
            allow_network,
        }
    }
}

/// Whether each requested resource control was installed for the workload.
/// The containing API member is nullable when execution evidence is unknown;
/// once present, every boolean is an explicit observed answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LimitEnforcement {
    pub wall_seconds: bool,
    pub cpu_seconds: bool,
    pub mem_mb: bool,
    pub max_pids: bool,
    pub max_file_mb: bool,
}

impl LimitEnforcement {
    pub const NONE: Self = Self {
        wall_seconds: false,
        cpu_seconds: false,
        mem_mb: false,
        max_pids: false,
        max_file_mb: false,
    };

    pub const DEVELOPMENT_SUBPROCESS: Self = Self {
        wall_seconds: true,
        ..Self::NONE
    };

    pub const NAMESPACE_SANDBOX: Self = Self {
        wall_seconds: true,
        cpu_seconds: true,
        mem_mb: true,
        max_pids: true,
        max_file_mb: true,
    };
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct JobSpec {
    pub language: String,
    pub code: String,
    #[serde(default)]
    pub stdin: Option<String>,
    #[serde(default)]
    pub limits: Limits,
    #[serde(default)]
    pub requirements: JobRequirements,
}

/// An execution spec whose controls describe effective, not merely requested,
/// policy. Unsupported or unactivated controls remain explicit `null` values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EffectiveJobSpec {
    pub language: String,
    pub code: String,
    pub stdin: Option<String>,
    pub limits: EffectiveLimits,
    #[serde(default)]
    pub requirements: JobRequirements,
    /// Null until a provider crosses its runtime-observed workload-ready
    /// boundary. A configured provider is not execution evidence.
    #[serde(default)]
    pub isolation_class: Option<IsolationClass>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isolation_satisfaction_is_monotonic_except_for_wasm_branch() {
        assert!(
            IsolationClass::GvisorApplicationKernel.satisfies(IsolationClass::LinuxSharedKernel)
        );
        assert!(IsolationClass::ConfidentialVm.satisfies(IsolationClass::HardwareVm));
        assert!(
            !IsolationClass::LinuxSharedKernel.satisfies(IsolationClass::GvisorApplicationKernel)
        );
        assert!(IsolationClass::WasmCapability.satisfies(IsolationClass::WasmCapability));
        assert!(!IsolationClass::HardwareVm.satisfies(IsolationClass::WasmCapability));
        assert!(!IsolationClass::WasmCapability.satisfies(IsolationClass::LinuxSharedKernel));
    }

    #[test]
    fn requirements_are_additive_and_default_to_none() {
        let spec: JobSpec = serde_json::from_value(serde_json::json!({
            "language": "python",
            "code": "print(1)"
        }))
        .unwrap();
        assert_eq!(spec.requirements.minimum_isolation, IsolationClass::None);

        let encoded = serde_json::to_value(JobRequirements {
            minimum_isolation: IsolationClass::GvisorApplicationKernel,
        })
        .unwrap();
        assert_eq!(encoded["minimum_isolation"], "gvisor-application-kernel");
    }
}
