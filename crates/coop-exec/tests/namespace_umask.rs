#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use std::path::PathBuf;

struct UmaskGuard(libc::mode_t);

impl UmaskGuard {
    fn set(mask: libc::mode_t) -> Self {
        // SAFETY: umask accepts every mode value and returns the prior mask.
        // This integration-test binary contains only this one test, so no
        // sibling thread can observe the temporary process-global setting.
        Self(unsafe { libc::umask(mask) })
    }
}

impl Drop for UmaskGuard {
    fn drop(&mut self) {
        // SAFETY: restoring the value returned by umask is always valid.
        unsafe {
            libc::umask(self.0);
        }
    }
}

/// A service-level UMask=0077 must not mask the explicitly public access
/// bits on the sandbox's private /dev/null and /dev/urandom nodes. The Python
/// preflight canary verifies their exact root:root metadata after dropping to
/// nobody, then reads urandom and writes null through the complete helper,
/// namespace, rootfs, cgroup, and seccomp path.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires root, delegated cgroup v2, ROOKHOLD_ROOTFS, and ROOKHOLD_SANDBOX_HELPER"]
async fn restrictive_umask_preserves_private_device_access() {
    let rootfs = PathBuf::from(
        std::env::var("ROOKHOLD_ROOTFS")
            .or_else(|_| std::env::var("COOP_ROOTFS"))
            .expect("ROOKHOLD_ROOTFS is required"),
    );
    let helper = PathBuf::from(
        std::env::var("ROOKHOLD_SANDBOX_HELPER")
            .or_else(|_| std::env::var("COOP_SANDBOX_HELPER"))
            .expect("ROOKHOLD_SANDBOX_HELPER is required"),
    );
    let jobs_root = std::env::temp_dir().join(format!(
        "coop-restrictive-umask-preflight-{}",
        std::process::id()
    ));
    std::fs::create_dir(&jobs_root).expect("create preflight jobs root");

    let _umask = UmaskGuard::set(0o077);
    coop_exec::namespace_sandbox_execution_preflight(
        &rootfs,
        &helper,
        &jobs_root,
        true,
        &[("python", Some("/usr/bin/python3"))],
    )
    .await
    .expect("restrictive inherited umask must not mask private device nodes");

    assert_eq!(
        std::fs::read_dir(&jobs_root).unwrap().count(),
        0,
        "device preflight must remove its disposable workdir"
    );
    std::fs::remove_dir(&jobs_root).expect("remove preflight jobs root");
}
