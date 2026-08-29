//! Trusted PID1 used inside a gVisor OCI sandbox.
//!
//! The helper writes a nonce-bound readiness frame only after the selected
//! interpreter has successfully crossed `execve`. It then remains PID1 to
//! reap orphans and kills every remaining descendant when the primary process
//! exits. The OCI provider supplies an inherited one-way pipe as FD 3; no host
//! filesystem socket or FIFO is exposed to the workload.

use nix::sys::signal::{kill, Signal};
use nix::sys::wait::{waitpid, WaitStatus};
use nix::unistd::{chdir, close, execve, fork, getgid, getuid, ForkResult, Pid};
use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

const READY_FD: i32 = 3;
const NONCE_HEX_LEN: usize = 48;
const READY_PREFIX: &[u8] = b"COOP_GVISOR_READY_V1\t";

pub fn main() -> i32 {
    match run(std::env::args_os().collect()) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("gVisor init bootstrap failed: {error}");
            126
        }
    }
}

fn run(args: Vec<std::ffi::OsString>) -> io::Result<i32> {
    if args.len() != 7 || args[1] != "--internal-v1" {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid coop-oci-init invocation",
        ));
    }
    let nonce = args[2]
        .to_str()
        .filter(|value| {
            value.len() == NONCE_HEX_LEN && value.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid readiness nonce"))?;
    let expected_uid = parse_identity(&args[3], "UID")?;
    let expected_gid = parse_identity(&args[4], "GID")?;
    if getuid().as_raw() != expected_uid || getgid().as_raw() != expected_gid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "OCI workload identity did not match the provider policy",
        ));
    }
    verify_no_new_privileges()?;
    verify_empty_capabilities()?;
    if !Path::new("/proc/gvisor/kernel_is_gvisor").is_file() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "gVisor runtime marker is absent",
        ));
    }

    let program = validate_internal_path(&args[5], false)?;
    let source = validate_internal_path(&args[6], true)?;
    chdir("/tmp").map_err(io::Error::other)?;
    set_cloexec(READY_FD)?;

    let mut exec_pipe = [0_i32; 2];
    if unsafe { libc::pipe2(exec_pipe.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(io::Error::last_os_error());
    }

    // SAFETY: this helper is a fresh, single-threaded process. The child uses
    // only async-signal-safe syscalls and allocation completed before fork.
    let forked = unsafe { fork() }.map_err(io::Error::other)?;
    let primary = match forked {
        ForkResult::Child => child_exec(exec_pipe, &program, &source),
        ForkResult::Parent { child } => child,
    };
    close(exec_pipe[1]).map_err(io::Error::other)?;

    let mut errno = [0_u8; std::mem::size_of::<i32>()];
    let read = loop {
        let result = unsafe { libc::read(exec_pipe[0], errno.as_mut_ptr().cast(), errno.len()) };
        if result < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
            continue;
        }
        break result;
    };
    let _ = close(exec_pipe[0]);
    if read < 0 {
        return Err(io::Error::last_os_error());
    }
    if read != 0 {
        let raw = i32::from_ne_bytes(errno);
        let _ = wait_for(primary);
        return Err(io::Error::from_raw_os_error(raw));
    }

    write_ready(nonce)?;
    let primary_status = wait_for_primary(primary)?;
    kill_descendants();
    reap_descendants();
    Ok(status_code(primary_status))
}

fn child_exec(exec_pipe: [i32; 2], program: &CString, source: &CString) -> ! {
    let _ = close(exec_pipe[0]);
    let _ = close(READY_FD);
    let _ = unsafe { libc::setpgid(0, 0) };
    let argv = [program, source];
    let env = [
        CString::new("PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin")
            .expect("static PATH"),
        CString::new("HOME=/tmp/home").expect("static HOME"),
        CString::new("TMPDIR=/tmp").expect("static TMPDIR"),
        CString::new("LANG=C.UTF-8").expect("static LANG"),
    ];
    let env_refs = env.iter().collect::<Vec<_>>();
    let error = execve(program, &argv, &env_refs).expect_err("execve replaces the child");
    let raw = error as i32;
    let bytes = raw.to_ne_bytes();
    let _ = unsafe { libc::write(exec_pipe[1], bytes.as_ptr().cast(), bytes.len()) };
    unsafe { libc::_exit(126) }
}

fn parse_identity(value: &std::ffi::OsStr, label: &str) -> io::Result<u32> {
    value
        .to_str()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|value| *value != 0)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid configured {label}"),
            )
        })
}

fn validate_internal_path(value: &std::ffi::OsStr, source: bool) -> io::Result<CString> {
    let path = Path::new(value);
    let valid = path.is_absolute()
        && !path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
        && (!source || path.starts_with("/work/"));
    if !valid {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid OCI program or source path",
        ));
    }
    CString::new(value.as_bytes())
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))
}

fn set_cloexec(fd: i32) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn verify_no_new_privileges() -> io::Result<()> {
    match unsafe { libc::prctl(libc::PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) } {
        1 => Ok(()),
        -1 => Err(io::Error::last_os_error()),
        _ => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "no_new_privs was not active",
        )),
    }
}

#[repr(C)]
struct CapHeader {
    version: u32,
    pid: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CapData {
    effective: u32,
    permitted: u32,
    inheritable: u32,
}

fn verify_empty_capabilities() -> io::Result<()> {
    let mut header = CapHeader {
        version: 0x2008_0522,
        pid: 0,
    };
    let mut data = [
        CapData {
            effective: 0,
            permitted: 0,
            inheritable: 0,
        },
        CapData {
            effective: 0,
            permitted: 0,
            inheritable: 0,
        },
    ];
    if unsafe {
        libc::syscall(
            libc::SYS_capget,
            &mut header as *mut CapHeader,
            data.as_mut_ptr(),
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    if data
        .iter()
        .any(|set| set.effective != 0 || set.permitted != 0 || set.inheritable != 0)
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "OCI workload retained Linux capabilities",
        ));
    }
    for capability in 0..=63 {
        let bounding = unsafe { libc::prctl(libc::PR_CAPBSET_READ, capability, 0, 0, 0) };
        if bounding == 1 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "OCI workload retained a capability in its bounding set",
            ));
        }
        if bounding < 0 && io::Error::last_os_error().raw_os_error() != Some(libc::EINVAL) {
            return Err(io::Error::last_os_error());
        }
        let ambient = unsafe {
            libc::prctl(
                libc::PR_CAP_AMBIENT,
                libc::PR_CAP_AMBIENT_IS_SET,
                capability,
                0,
                0,
            )
        };
        if ambient == 1 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "OCI workload retained an ambient capability",
            ));
        }
        if ambient < 0 && io::Error::last_os_error().raw_os_error() != Some(libc::EINVAL) {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

fn write_ready(nonce: &str) -> io::Result<()> {
    let mut frame = Vec::with_capacity(READY_PREFIX.len() + nonce.len() + 1);
    frame.extend_from_slice(READY_PREFIX);
    frame.extend_from_slice(nonce.as_bytes());
    frame.push(b'\n');
    let mut written = 0;
    while written < frame.len() {
        let result = unsafe {
            libc::write(
                READY_FD,
                frame[written..].as_ptr().cast(),
                frame.len() - written,
            )
        };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        written += result as usize;
    }
    close(READY_FD).map_err(io::Error::other)
}

fn wait_for(pid: Pid) -> io::Result<WaitStatus> {
    loop {
        match waitpid(pid, None) {
            Ok(status) => return Ok(status),
            Err(nix::errno::Errno::EINTR) => continue,
            Err(error) => return Err(io::Error::other(error)),
        }
    }
}

fn wait_for_primary(primary: Pid) -> io::Result<WaitStatus> {
    loop {
        let status = loop {
            match waitpid(Pid::from_raw(-1), None) {
                Ok(status) => break status,
                Err(nix::errno::Errno::EINTR) => continue,
                Err(error) => return Err(io::Error::other(error)),
            }
        };
        match status {
            WaitStatus::Exited(pid, _) | WaitStatus::Signaled(pid, _, _) if pid == primary => {
                return Ok(status)
            }
            _ => continue,
        }
    }
}

fn kill_descendants() {
    if let Err(error) = kill(Pid::from_raw(-1), Signal::SIGKILL) {
        if error != nix::errno::Errno::ESRCH {
            eprintln!("gVisor init could not kill descendants: {error}");
        }
    }
}

fn reap_descendants() {
    loop {
        match waitpid(Pid::from_raw(-1), None) {
            Ok(_) | Err(nix::errno::Errno::EINTR) => continue,
            Err(nix::errno::Errno::ECHILD) => break,
            Err(_) => break,
        }
    }
}

fn status_code(status: WaitStatus) -> i32 {
    match status {
        WaitStatus::Exited(_, code) => code,
        WaitStatus::Signaled(_, signal, _) => 128 + signal as i32,
        _ => 126,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_paths_are_absolute_and_source_is_work_scoped() {
        assert!(validate_internal_path(std::ffi::OsStr::new("/usr/bin/python3"), false).is_ok());
        assert!(validate_internal_path(std::ffi::OsStr::new("/work/job.py"), true).is_ok());
        assert!(validate_internal_path(std::ffi::OsStr::new("/tmp/job.py"), true).is_err());
        assert!(validate_internal_path(std::ffi::OsStr::new("../bin/sh"), false).is_err());
    }
}
