//! Seccomp-BPF syscall allowlist for sandboxed jobs (audit finding F-005).
//!
//! Design (installed in the forked child right before `execve`, so the
//! interpreter and everything it spawns inherit the filter):
//!
//! - **allowlist**: an explicit common set of syscalls needed by CPython,
//!   Node, and bash for ordinary compute/file work executes normally;
//! - **kill list**: notorious kernel-attack-surface syscalls (`ptrace`, `bpf`,
//!   module loading, keyrings, namespace escapes, …) return
//!   `SECCOMP_RET_KILL_PROCESS`. Unlike `TRAP`, this cannot be caught by a
//!   tenant-installed SIGSYS handler;
//! - **explicit errno deny list**: capability probes for disabled facilities
//!   such as io_uring receive `EPERM`, allowing runtimes to fall back without
//!   making the kernel facility usable;
//! - **everything else**: `SECCOMP_RET_ERRNO(ENOSYS)` instead of a kill, so
//!   glibc/Node feature probes (`clone3`, `faccessat2`, `epoll_pwait2`, …)
//!   fall back to older syscalls the way they do on old kernels;
//! - **`socket()` argument filter**: only `AF_UNIX` sockets may be created;
//!   other families fail with `EAFNOSUPPORT` (defense in depth on top of the
//!   empty network namespace);
//! - **wrong architecture** (incl. x32 ABI confusion) is trapped.
//!
//! x86_64 only today; the architecture check makes other-ABI binaries fail
//! closed rather than run with mismatched syscall numbers.
//!
//! The filter is installed *after* all privileged bootstrap work (mounts,
//! cgroup attach, id maps) and before the interpreter starts, so setup
//! syscalls never hit it. `PR_SET_NO_NEW_PRIVS` is set first, which also
//! guarantees the filter cannot be bypassed via execve of a setuid binary.

// x86_64 syscall numbers (asm/unistd_64.h). Kept as plain constants rather
// than pulling in a syscall crate — the supply chain stays unchanged.
// Some numbers are kept for documentation/completeness even when unreferenced.
#[allow(non_upper_case_globals, dead_code)]
mod nr {
    pub const read: u32 = 0;
    pub const write: u32 = 1;
    pub const open: u32 = 2;
    pub const close: u32 = 3;
    pub const stat: u32 = 4;
    pub const fstat: u32 = 5;
    pub const lstat: u32 = 6;
    pub const poll: u32 = 7;
    pub const lseek: u32 = 8;
    pub const mmap: u32 = 9;
    pub const mprotect: u32 = 10;
    pub const munmap: u32 = 11;
    pub const brk: u32 = 12;
    pub const rt_sigaction: u32 = 13;
    pub const rt_sigprocmask: u32 = 14;
    pub const rt_sigreturn: u32 = 15;
    pub const ioctl: u32 = 16;
    pub const pread64: u32 = 17;
    pub const pwrite64: u32 = 18;
    pub const readv: u32 = 19;
    pub const writev: u32 = 20;
    pub const access: u32 = 21;
    pub const pipe: u32 = 22;
    pub const select: u32 = 23;
    pub const sched_yield: u32 = 24;
    pub const mremap: u32 = 25;
    pub const msync: u32 = 26;
    pub const mincore: u32 = 27;
    pub const madvise: u32 = 28;
    pub const dup: u32 = 32;
    pub const dup2: u32 = 33;
    pub const pause: u32 = 34;
    pub const nanosleep: u32 = 35;
    pub const getitimer: u32 = 36;
    pub const alarm: u32 = 37;
    pub const setitimer: u32 = 38;
    pub const getpid: u32 = 39;
    pub const socket: u32 = 41;
    pub const socketpair: u32 = 53;
    pub const clone: u32 = 56;
    pub const fork: u32 = 57;
    pub const vfork: u32 = 58;
    pub const execve: u32 = 59;
    pub const exit: u32 = 60;
    pub const wait4: u32 = 61;
    pub const kill: u32 = 62;
    pub const uname: u32 = 63;
    pub const fcntl: u32 = 72;
    pub const flock: u32 = 73;
    pub const fsync: u32 = 74;
    pub const fdatasync: u32 = 75;
    pub const truncate: u32 = 76;
    pub const ftruncate: u32 = 77;
    pub const getdents: u32 = 78;
    pub const getcwd: u32 = 79;
    pub const chdir: u32 = 80;
    pub const fchdir: u32 = 81;
    pub const rename: u32 = 82;
    pub const mkdir: u32 = 83;
    pub const rmdir: u32 = 84;
    pub const creat: u32 = 85;
    pub const link: u32 = 86;
    pub const unlink: u32 = 87;
    pub const symlink: u32 = 88;
    pub const readlink: u32 = 89;
    pub const chmod: u32 = 90;
    pub const fchmod: u32 = 91;
    pub const chown: u32 = 92;
    pub const fchown: u32 = 93;
    pub const lchown: u32 = 94;
    pub const umask: u32 = 95;
    pub const gettimeofday: u32 = 96;
    pub const getrlimit: u32 = 97;
    pub const getrusage: u32 = 98;
    pub const sysinfo: u32 = 99;
    pub const times: u32 = 100;
    pub const getuid: u32 = 102;
    pub const getgid: u32 = 104;
    pub const geteuid: u32 = 107;
    pub const getegid: u32 = 108;
    pub const setpgid: u32 = 109;
    pub const getppid: u32 = 110;
    pub const getpgrp: u32 = 111;
    pub const setsid: u32 = 112;
    pub const getgroups: u32 = 115;
    pub const getresuid: u32 = 118;
    pub const getresgid: u32 = 120;
    pub const getpgid: u32 = 121;
    pub const getsid: u32 = 124;
    pub const capget: u32 = 125;
    pub const rt_sigpending: u32 = 127;
    pub const rt_sigtimedwait: u32 = 128;
    pub const rt_sigqueueinfo: u32 = 129;
    pub const rt_sigsuspend: u32 = 130;
    pub const sigaltstack: u32 = 131;
    pub const statfs: u32 = 137;
    pub const fstatfs: u32 = 138;
    pub const getpriority: u32 = 140;
    pub const setpriority: u32 = 141;
    pub const sched_getparam: u32 = 143;
    pub const sched_getscheduler: u32 = 145;
    pub const sched_get_priority_max: u32 = 146;
    pub const sched_get_priority_min: u32 = 147;
    pub const sched_rr_get_interval: u32 = 148;
    pub const prctl: u32 = 157;
    pub const arch_prctl: u32 = 158;
    pub const setrlimit: u32 = 160;
    pub const readahead: u32 = 187;
    pub const gettid: u32 = 186;
    pub const time: u32 = 201;
    pub const futex: u32 = 202;
    pub const sched_getaffinity: u32 = 204;
    pub const getdents64: u32 = 217;
    pub const set_tid_address: u32 = 218;
    pub const restart_syscall: u32 = 219;
    pub const fadvise64: u32 = 221;
    pub const clock_gettime: u32 = 228;
    pub const clock_getres: u32 = 229;
    pub const clock_nanosleep: u32 = 230;
    pub const exit_group: u32 = 231;
    pub const epoll_wait: u32 = 232;
    pub const epoll_ctl: u32 = 233;
    pub const tgkill: u32 = 234;
    pub const utimes: u32 = 235;
    pub const waitid: u32 = 247;
    pub const openat: u32 = 257;
    pub const mkdirat: u32 = 258;
    pub const newfstatat: u32 = 262;
    pub const unlinkat: u32 = 263;
    pub const renameat: u32 = 264;
    pub const linkat: u32 = 265;
    pub const symlinkat: u32 = 266;
    pub const readlinkat: u32 = 267;
    pub const fchmodat: u32 = 268;
    pub const faccessat: u32 = 269;
    pub const pselect6: u32 = 270;
    pub const ppoll: u32 = 271;
    pub const set_robust_list: u32 = 273;
    pub const splice: u32 = 275;
    pub const tee: u32 = 276;
    pub const sync_file_range: u32 = 277;
    pub const vmsplice: u32 = 278;
    pub const utimensat: u32 = 280;
    pub const epoll_pwait: u32 = 281;
    pub const signalfd: u32 = 282;
    pub const timerfd_create: u32 = 283;
    pub const eventfd: u32 = 284;
    pub const fallocate: u32 = 285;
    pub const timerfd_settime: u32 = 286;
    pub const timerfd_gettime: u32 = 287;
    pub const accept4: u32 = 288;
    pub const signalfd4: u32 = 289;
    pub const eventfd2: u32 = 290;
    pub const epoll_create1: u32 = 291;
    pub const dup3: u32 = 292;
    pub const pipe2: u32 = 293;
    pub const preadv: u32 = 295;
    pub const pwritev: u32 = 296;
    pub const rt_tgsigqueueinfo: u32 = 297;
    pub const perf_event_open: u32 = 298;
    pub const recvmmsg: u32 = 299;
    pub const prlimit64: u32 = 302;
    pub const syncfs: u32 = 306;
    pub const getcpu: u32 = 309;
    pub const sched_getattr: u32 = 315;
    pub const renameat2: u32 = 316;
    pub const getrandom: u32 = 318;
    pub const memfd_create: u32 = 319;
    pub const membarrier: u32 = 324;
    pub const copy_file_range: u32 = 326;
    pub const preadv2: u32 = 327;
    pub const pwritev2: u32 = 328;
    pub const statx: u32 = 332;
    pub const rseq: u32 = 334;
    pub const io_uring_setup: u32 = 425;
    pub const io_uring_enter: u32 = 426;
    pub const io_uring_register: u32 = 427;
    pub const pidfd_getfd: u32 = 437;
    pub const faccessat2: u32 = 438;
    // trap-list-only numbers
    pub const ptrace: u32 = 101;
    pub const syslog: u32 = 103;
    pub const setuid: u32 = 105;
    pub const setgid: u32 = 106;
    pub const setreuid: u32 = 113;
    pub const setregid: u32 = 114;
    pub const setgroups: u32 = 116;
    pub const setresuid: u32 = 117;
    pub const setresgid: u32 = 119;
    pub const setfsuid: u32 = 122;
    pub const setfsgid: u32 = 123;
    pub const ustat: u32 = 136;
    pub const sysfs_nr: u32 = 139;
    pub const mknod: u32 = 133;
    pub const uselib: u32 = 134;
    pub const personality: u32 = 135;
    pub const vhangup: u32 = 153;
    pub const modify_ldt: u32 = 154;
    pub const pivot_root: u32 = 155;
    pub const _sysctl: u32 = 156;
    pub const adjtimex: u32 = 159;
    pub const chroot: u32 = 161;
    pub const acct: u32 = 163;
    pub const settimeofday: u32 = 164;
    pub const mount: u32 = 165;
    pub const umount2: u32 = 166;
    pub const swapon: u32 = 167;
    pub const swapoff: u32 = 168;
    pub const reboot: u32 = 169;
    pub const sethostname: u32 = 170;
    pub const setdomainname: u32 = 171;
    pub const iopl: u32 = 172;
    pub const ioperm: u32 = 173;
    pub const create_module: u32 = 174;
    pub const init_module: u32 = 175;
    pub const delete_module: u32 = 176;
    pub const quotactl: u32 = 179;
    pub const lookup_dcookie: u32 = 212;
    pub const remap_file_pages: u32 = 216;
    pub const mbind: u32 = 237;
    pub const set_mempolicy: u32 = 238;
    pub const get_mempolicy: u32 = 239;
    pub const kexec_load: u32 = 246;
    pub const add_key: u32 = 248;
    pub const request_key: u32 = 249;
    pub const keyctl: u32 = 250;
    pub const migrate_pages: u32 = 256;
    pub const move_pages: u32 = 279;
    pub const fanotify_init: u32 = 300;
    pub const fanotify_mark: u32 = 301;
    pub const process_vm_readv: u32 = 310;
    pub const process_vm_writev: u32 = 311;
    pub const kcmp: u32 = 312;
    pub const finit_module: u32 = 313;
    pub const kexec_file_load: u32 = 320;
    pub const bpf: u32 = 321;
    pub const userfaultfd: u32 = 323;
    pub const clock_adjtime: u32 = 305;
    pub const open_tree: u32 = 428;
    pub const move_mount: u32 = 429;
    pub const fsopen: u32 = 430;
    pub const fsmount: u32 = 431;
    pub const fspick: u32 = 432;
    pub const landlock_create_ruleset: u32 = 444;
    pub const landlock_add_rule: u32 = 445;
    pub const landlock_restrict_self: u32 = 446;
    pub const memfd_secret: u32 = 447;
    // referenced by the allowlist tables
    pub const inotify_add_watch: u32 = 254;
    pub const inotify_init: u32 = 253;
    pub const inotify_init1: u32 = 294;
    pub const inotify_rm_watch: u32 = 255;
    pub const mq_getsetattr: u32 = 245;
    pub const mq_notify: u32 = 244;
    pub const mq_open: u32 = 240;
    pub const mq_timedreceive: u32 = 243;
    pub const mq_timedsend: u32 = 242;
    pub const mq_unlink: u32 = 241;
    pub const process_madvise: u32 = 439;
    pub const quotactl_fd: u32 = 443;
    pub const setns: u32 = 308;
}

/// Syscalls every interpreter may use for ordinary compute + file work.
/// The generated program is a linear comparison chain, so order carries no
/// semantics; tests assert there are no duplicates and no overlap with TRAP.
const ALLOWED: &[u32] = &[
    nr::access,
    nr::alarm,
    nr::arch_prctl,
    nr::brk,
    nr::capget,
    nr::chdir,
    nr::clock_getres,
    nr::clock_gettime,
    nr::clock_nanosleep,
    nr::clone,
    nr::close,
    nr::dup,
    nr::dup2,
    nr::dup3,
    nr::epoll_create1,
    nr::epoll_ctl,
    nr::epoll_pwait,
    nr::epoll_wait,
    nr::eventfd2,
    nr::execve,
    nr::exit,
    nr::exit_group,
    nr::faccessat,
    nr::fadvise64,
    nr::fchmod,
    nr::fchown,
    nr::fcntl,
    nr::fdatasync,
    nr::flock,
    nr::fork,
    nr::fstat,
    nr::fstatfs,
    nr::fsync,
    nr::ftruncate,
    nr::futex,
    nr::getcwd,
    nr::getdents,
    nr::getdents64,
    nr::getegid,
    nr::geteuid,
    nr::getgid,
    nr::getgroups,
    nr::getitimer,
    nr::getpgid,
    nr::getpgrp,
    nr::getpid,
    nr::getppid,
    nr::getpriority,
    nr::getrandom,
    nr::getresgid,
    nr::getresuid,
    nr::getrlimit,
    nr::getrusage,
    nr::getsid,
    nr::gettid,
    nr::gettimeofday,
    nr::getuid,
    nr::inotify_add_watch,
    nr::inotify_init,
    nr::inotify_init1,
    nr::inotify_rm_watch,
    nr::ioctl,
    nr::kill,
    nr::link,
    nr::linkat,
    nr::lseek,
    nr::lstat,
    nr::madvise,
    nr::membarrier,
    nr::memfd_create,
    nr::mincore,
    nr::mkdir,
    nr::mkdirat,
    nr::mmap,
    nr::mprotect,
    nr::mq_getsetattr,
    nr::mq_notify,
    nr::mq_open,
    nr::mq_timedreceive,
    nr::mq_timedsend,
    nr::mq_unlink,
    nr::mremap,
    nr::msync,
    nr::munmap,
    nr::nanosleep,
    nr::newfstatat,
    nr::open,
    nr::openat,
    nr::pause,
    nr::pipe,
    nr::pipe2,
    nr::poll,
    nr::ppoll,
    nr::prctl,
    nr::pread64,
    nr::preadv,
    nr::preadv2,
    nr::prlimit64,
    nr::pselect6,
    nr::pwrite64,
    nr::pwritev,
    nr::pwritev2,
    nr::read,
    nr::readahead,
    nr::readlink,
    nr::readlinkat,
    nr::readv,
    nr::rename,
    nr::renameat,
    nr::renameat2,
    nr::restart_syscall,
    nr::rmdir,
    nr::rseq,
    nr::rt_sigaction,
    nr::rt_sigpending,
    nr::rt_sigprocmask,
    nr::rt_sigqueueinfo,
    nr::rt_sigreturn,
    nr::rt_sigsuspend,
    nr::rt_sigtimedwait,
    nr::sched_get_priority_max,
    nr::sched_get_priority_min,
    nr::sched_getaffinity,
    nr::sched_getparam,
    nr::sched_getscheduler,
    nr::sched_yield,
    nr::select,
    nr::set_robust_list,
    nr::set_tid_address,
    nr::setitimer,
    nr::setpgid,
    nr::setpriority,
    nr::setrlimit,
    nr::setsid,
    nr::sigaltstack,
    nr::stat,
    nr::statfs,
    nr::statx,
    nr::sysinfo,
    nr::tgkill,
    nr::time,
    nr::times,
    nr::truncate,
    nr::umask,
    nr::uname,
    nr::unlink,
    nr::unlinkat,
    nr::utimensat,
    nr::utimes,
    nr::vfork,
    nr::wait4,
    nr::waitid,
    nr::write,
    nr::writev,
];

/// Notorious kernel-attack-surface syscalls: module loading, BPF, ptrace,
/// keyrings, namespace manipulation, raw clock/hostname control…
/// These return `SECCOMP_RET_KILL_PROCESS` and cannot be caught instead of
/// a silent `ENOSYS` that a probe could mistake for "unsupported".
const TRAP: &[u32] = &[
    nr::acct,
    nr::add_key,
    nr::bpf,
    nr::chroot,
    nr::clock_adjtime,
    nr::create_module,
    nr::delete_module,
    nr::fanotify_init,
    nr::fanotify_mark,
    nr::finit_module,
    nr::fsopen,
    nr::fsmount,
    nr::fspick,
    nr::init_module,
    nr::ioperm,
    nr::iopl,
    nr::kcmp,
    nr::kexec_file_load,
    nr::kexec_load,
    nr::keyctl,
    nr::landlock_add_rule,
    nr::landlock_create_ruleset,
    nr::landlock_restrict_self,
    nr::lookup_dcookie,
    nr::mbind,
    nr::memfd_secret,
    nr::migrate_pages,
    nr::modify_ldt,
    nr::mount,
    nr::move_mount,
    nr::move_pages,
    nr::open_tree,
    nr::perf_event_open,
    nr::personality,
    nr::pidfd_getfd,
    nr::pivot_root,
    nr::process_madvise,
    nr::process_vm_readv,
    nr::process_vm_writev,
    nr::ptrace,
    nr::quotactl,
    nr::quotactl_fd,
    nr::reboot,
    nr::remap_file_pages,
    nr::request_key,
    nr::set_mempolicy,
    nr::setdomainname,
    nr::setfsuid,
    nr::setfsgid,
    nr::setgid,
    nr::setgroups,
    nr::sethostname,
    nr::setns,
    nr::setregid,
    nr::setreuid,
    nr::setresgid,
    nr::setresuid,
    nr::settimeofday,
    nr::setuid,
    nr::swapoff,
    nr::swapon,
    nr::sysfs_nr,
    nr::syslog,
    nr::umount2,
    nr::uselib,
    nr::userfaultfd,
    nr::ustat,
    nr::vhangup,
];

/// Disabled kernel facilities that legitimate runtimes probe during startup.
/// Returning EPERM preserves feature-detection fallback while ensuring neither
/// ring creation nor use of an inherited io_uring descriptor is possible.
const ERRNO_DENY: &[u32] = &[
    nr::io_uring_enter,
    nr::io_uring_register,
    nr::io_uring_setup,
];

// seccomp_data offsets on x86_64 (little-endian):
//   offset 0 -> nr (u32);  offset 4 -> arch (u32);  offset 16 -> args[0] low
const OFFSET_NR: u8 = 0;
const OFFSET_ARG0_LO: u8 = 16;
const ARCH_AUDIT_X86_64: u32 = 0xC000_003E;
const AF_UNIX: u32 = 1;

// libc's BPF_* constants are u32 while sock_filter.code is u16, so the
// helpers take u32 opcodes and narrow once at construction.
fn bpf_stmt(code: u32, k: u32) -> libc::sock_filter {
    libc::sock_filter {
        code: code as u16,
        jt: 0,
        jf: 0,
        k,
    }
}

fn bpf_jump(code: u32, k: u32, jt: u8, jf: u8) -> libc::sock_filter {
    libc::sock_filter {
        code: code as u16,
        jt,
        jf,
        k,
    }
}

#[allow(non_snake_case)]
fn allow() -> libc::sock_filter {
    bpf_stmt(libc::BPF_RET | libc::BPF_K, libc::SECCOMP_RET_ALLOW)
}

#[allow(non_snake_case)]
fn errno_ret(err: i32) -> libc::sock_filter {
    bpf_stmt(
        libc::BPF_RET | libc::BPF_K,
        libc::SECCOMP_RET_ERRNO | err as u32,
    )
}

#[allow(non_snake_case)]
fn kill_process() -> libc::sock_filter {
    bpf_stmt(libc::BPF_RET | libc::BPF_K, libc::SECCOMP_RET_KILL_PROCESS)
}

/// Build the complete filter program. Pure so tests can validate its shape.
pub fn build_filter() -> Vec<libc::sock_filter> {
    let mut prog =
        Vec::with_capacity(ALLOWED.len() * 2 + TRAP.len() * 2 + ERRNO_DENY.len() * 2 + 18);

    // Wrong architecture (x32 included) → kill loudly.
    prog.push(bpf_stmt(libc::BPF_LD | libc::BPF_W | libc::BPF_ABS, 4));
    prog.push(bpf_jump(
        libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K,
        ARCH_AUDIT_X86_64,
        1,
        0,
    ));
    prog.push(kill_process());

    // Load syscall number.
    prog.push(bpf_stmt(
        libc::BPF_LD | libc::BPF_W | libc::BPF_ABS,
        u32::from(OFFSET_NR),
    ));

    // x32 shares AUDIT_ARCH_X86_64 but uses a different syscall-number ABI.
    // Reject it uncatchably before comparing any x86_64 syscall table entry.
    prog.push(bpf_jump(
        libc::BPF_JMP | libc::BPF_JSET | libc::BPF_K,
        0x4000_0000,
        0,
        1,
    ));
    prog.push(kill_process());

    // Trap list first: notorious surface dies regardless of arguments.
    for &n in TRAP {
        prog.push(bpf_jump(
            libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K,
            n,
            0,
            1,
        ));
        prog.push(kill_process());
    }

    // Explicit errno-deny list: the facility remains unavailable, but a
    // capability probe can observe the denial and select a safe fallback.
    for &n in ERRNO_DENY {
        prog.push(bpf_jump(
            libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K,
            n,
            0,
            1,
        ));
        prog.push(errno_ret(libc::EPERM));
    }

    // clone() with namespace-creating flags is trapped (deep-hunt note on
    // the “namespace escapes … trap” doc claim — plain clone/fork/vfork
    // threads are still allowed; only CLONE_NEW* is loud).
    // Flags live in arg0 low word on x86_64; mask the NEW* bits and trap
    // when any are set.
    const CLONE_NS_MASK: u32 = 0x7E02_0080; // all CLONE_NEW* incl. CGROUP/TIME
    prog.push(bpf_jump(
        libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K,
        nr::clone,
        0,
        5,
    ));
    prog.push(bpf_stmt(
        libc::BPF_LD | libc::BPF_W | libc::BPF_ABS,
        u32::from(OFFSET_ARG0_LO),
    ));
    prog.push(bpf_stmt(
        libc::BPF_ALU | libc::BPF_AND | libc::BPF_K,
        CLONE_NS_MASK,
    ));
    prog.push(bpf_jump(
        libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K,
        0,
        1,
        0,
    ));
    prog.push(kill_process());
    prog.push(allow());

    // socket(domain) and socketpair(domain): AF_UNIX allowed; every other
    // family gets EAFNOSUPPORT.
    prog.push(bpf_jump(
        libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K,
        nr::socket,
        1,
        0,
    ));
    prog.push(bpf_jump(
        libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K,
        nr::socketpair,
        0,
        4,
    ));
    prog.push(bpf_stmt(
        libc::BPF_LD | libc::BPF_W | libc::BPF_ABS,
        u32::from(OFFSET_ARG0_LO),
    ));
    prog.push(bpf_jump(
        libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K,
        AF_UNIX,
        1,
        0,
    ));
    prog.push(errno_ret(libc::EAFNOSUPPORT));
    prog.push(allow());

    // Allowlist: jt=0 lands on the paired allow() on match; jf=1 continues
    // to the next comparison on mismatch (same shape as the trap chain).
    for &n in ALLOWED {
        prog.push(bpf_jump(
            libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K,
            n,
            0,
            1,
        ));
        prog.push(allow());
    }

    // Default: ENOSYS so glibc fallback chains (clone3→clone, faccessat2→…)
    // behave like they do on an old kernel instead of killing the process.
    prog.push(errno_ret(libc::ENOSYS));
    prog
}

/// Install the filter on the current thread (the forked child is single-
/// threaded). Fail-closed: an installation error aborts bootstrap rather
/// than running unfiltered.
pub fn install() -> Result<(), String> {
    let prog = build_filter();
    let mut fprog = libc::sock_fprog {
        len: prog.len() as u16,
        filter: prog.as_ptr() as *mut libc::sock_filter,
    };
    if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
        return Err("PR_SET_NO_NEW_PRIVS failed".to_string());
    }
    let rc = unsafe {
        libc::syscall(
            libc::SYS_seccomp,
            libc::SECCOMP_SET_MODE_FILTER,
            0,
            &mut fprog,
        )
    };
    if rc != 0 {
        return Err("seccomp(SECCOMP_SET_MODE_FILTER) failed".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_are_unique_and_pairwise_disjoint() {
        // Order carries no semantics in a linear JEQ chain, but duplicates
        // and overlap between verdict tables would silently weaken intent.
        for (name, table) in [
            ("ALLOWED", ALLOWED),
            ("TRAP", TRAP),
            ("ERRNO_DENY", ERRNO_DENY),
        ] {
            let mut entries = table.to_vec();
            entries.sort_unstable();
            assert!(
                entries.windows(2).all(|window| window[0] != window[1]),
                "{name} must not contain duplicates"
            );
        }
        for call in ALLOWED {
            assert!(!TRAP.contains(call), "syscall {call} overlaps TRAP");
            assert!(
                !ERRNO_DENY.contains(call),
                "syscall {call} overlaps ERRNO_DENY"
            );
        }
        for call in TRAP {
            assert!(
                !ERRNO_DENY.contains(call),
                "syscall {call} overlaps ERRNO_DENY"
            );
        }
    }

    #[test]
    fn dangerous_calls_are_never_allowed() {
        for call in [
            nr::ptrace,
            nr::bpf,
            nr::init_module,
            nr::keyctl,
            nr::setns,
            nr::userfaultfd,
            nr::perf_event_open,
            nr::mount,
        ] {
            assert!(!ALLOWED.contains(&call), "{call} must not be allowed");
            assert!(TRAP.contains(&call), "{call} must be trapped");
        }
        for call in [
            nr::io_uring_setup,
            nr::io_uring_enter,
            nr::io_uring_register,
        ] {
            assert!(!ALLOWED.contains(&call), "{call} must not be allowed");
            assert!(
                ERRNO_DENY.contains(&call),
                "{call} must be explicitly errno-denied"
            );
        }
    }

    #[test]
    fn interpreters_core_calls_are_allowed() {
        for call in [
            nr::read,
            nr::write,
            nr::openat,
            nr::mmap,
            nr::clone,
            nr::execve,
            nr::futex,
            nr::getrandom,
            nr::clock_gettime,
            nr::wait4,
        ] {
            assert!(ALLOWED.contains(&call), "{call} must be allowed");
        }
    }

    /// Every jump target must stay inside the program and the program must
    /// end with a RET — catches off-by-one jump bugs without running BPF.
    #[test]
    fn filter_program_is_structurally_sound() {
        let prog = build_filter();
        assert!(prog.len() > 10);
        assert_eq!(
            prog.last().unwrap().code & (libc::BPF_RET as u16),
            libc::BPF_RET as u16,
            "program must end with a RET"
        );
        for (i, insn) in prog.iter().enumerate() {
            if insn.code & (libc::BPF_JMP as u16) == libc::BPF_JMP as u16 {
                assert!(
                    i + 1 + (insn.jt as usize) < prog.len(),
                    "jt of insn {i} escapes the program"
                );
                assert!(
                    i + 1 + (insn.jf as usize) < prog.len(),
                    "jf of insn {i} escapes the program"
                );
            }
        }
    }

    /// Walk the program exactly like the kernel would, feeding it a synthetic
    /// seccomp_data (native arch + given nr/arg0) and returning the verdict.
    fn simulate(nr_val: u32, arg0: u32) -> u32 {
        let prog = build_filter();
        const CLASS_MASK: u16 = 0x07;
        const CLASS_LD: u16 = libc::BPF_LD as u16;
        const CLASS_JMP: u16 = libc::BPF_JMP as u16;
        const CLASS_RET: u16 = libc::BPF_RET as u16;
        const CLASS_ALU: u16 = libc::BPF_ALU as u16;
        let mut acc: u32 = 0; // last BPF_ABS load / ALU accumulator
        let mut ip = 0usize;
        loop {
            let insn = &prog[ip];
            match insn.code & CLASS_MASK {
                CLASS_LD => {
                    acc = match insn.k {
                        0 => nr_val,
                        4 => ARCH_AUDIT_X86_64,
                        16 => arg0,
                        other => panic!("unexpected ABS offset {other}"),
                    };
                    ip += 1;
                }
                CLASS_ALU => {
                    // Only AND K is used by the filter (clone namespace mask).
                    // BPF_AND = 0x50; combined code = ALU(0x04) | AND(0x50) | K(0x00)=0x54
                    acc &= insn.k;
                    ip += 1;
                }
                CLASS_JMP => {
                    let matches = if insn.code & 0xf0 == libc::BPF_JSET as u16 {
                        acc & insn.k != 0
                    } else {
                        acc == insn.k
                    };
                    ip += if matches {
                        1 + insn.jt as usize
                    } else {
                        1 + insn.jf as usize
                    };
                }
                CLASS_RET => return insn.k,
                other => panic!("unexpected BPF class {other:#x}"),
            }
        }
    }

    #[test]
    fn simulated_verdicts_match_policy() {
        // ordinary calls → ALLOW
        assert_eq!(simulate(nr::write, 0), libc::SECCOMP_RET_ALLOW);
        assert_eq!(simulate(nr::openat, 0), libc::SECCOMP_RET_ALLOW);
        // kill list → KILL_PROCESS, which cannot be caught by the workload.
        let trapped = simulate(nr::ptrace, 0);
        assert_eq!(trapped & 0xffff_0000, libc::SECCOMP_RET_KILL_PROCESS);
        let bpf_trapped = simulate(nr::bpf, 0);
        assert_eq!(bpf_trapped & 0xffff_0000, libc::SECCOMP_RET_KILL_PROCESS);
        // io_uring stays unavailable, but runtime capability probes survive.
        for call in [
            nr::io_uring_setup,
            nr::io_uring_enter,
            nr::io_uring_register,
        ] {
            let denied = simulate(call, 0);
            assert_eq!(denied & 0x7fff_0000, libc::SECCOMP_RET_ERRNO);
            assert_eq!(denied & 0xffff, libc::EPERM as u32);
        }
        // unknown-but-harmless → ENOSYS
        let verdict = simulate(9999, 0);
        assert_eq!(verdict & 0x7fff_0000, libc::SECCOMP_RET_ERRNO);
        assert_eq!(verdict & 0xffff, libc::ENOSYS as u32);
        // socket/socketpair(AF_UNIX) → ALLOW; exotic domains → EAFNOSUPPORT
        assert_eq!(simulate(nr::socket, AF_UNIX), libc::SECCOMP_RET_ALLOW);
        let inet = simulate(nr::socket, 2);
        assert_eq!(inet & 0x7fff_0000, libc::SECCOMP_RET_ERRNO);
        assert_eq!(inet & 0xffff, libc::EAFNOSUPPORT as u32);
        assert_eq!(simulate(nr::socketpair, AF_UNIX), libc::SECCOMP_RET_ALLOW);
        for domain in [2, 30] {
            let exotic_pair = simulate(nr::socketpair, domain);
            assert_eq!(exotic_pair & 0x7fff_0000, libc::SECCOMP_RET_ERRNO);
            assert_eq!(exotic_pair & 0xffff, libc::EAFNOSUPPORT as u32);
        }
        // clone() without namespace flags → ALLOW; with NEW* → TRAP
        assert_eq!(simulate(nr::clone, 0), libc::SECCOMP_RET_ALLOW);
        let ns_clone = simulate(nr::clone, 0x1000_0000); // CLONE_NEWUSER
        assert_eq!(ns_clone & 0xffff_0000, libc::SECCOMP_RET_KILL_PROCESS);
        let ns_net = simulate(nr::clone, 0x4000_0000); // CLONE_NEWNET
        assert_eq!(ns_net & 0xffff_0000, libc::SECCOMP_RET_KILL_PROCESS);
        let ns_time = simulate(nr::clone, 0x80); // CLONE_NEWTIME
        assert_eq!(ns_time & 0xffff_0000, libc::SECCOMP_RET_KILL_PROCESS);
        let x32 = simulate(0x4000_0000 | nr::write, 0);
        assert_eq!(x32 & 0xffff_0000, libc::SECCOMP_RET_KILL_PROCESS);
    }
}
