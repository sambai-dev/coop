//! Seccomp diagnostic (Linux only): installs the production filter and walks
//! every syscall category an interpreter touches during startup. The last
//! `step:` line printed before the process dies with SIGSYS names the exact
//! policy bug. Run with:
//!   cargo run -p coop-exec --example seccomp_diag
fn main() {
    #[cfg(target_os = "linux")]
    run();

    #[cfg(not(target_os = "linux"))]
    eprintln!("seccomp diagnostics are Linux-only");
}

#[cfg(target_os = "linux")]
fn run() {
    use coop_exec::seccomp;

    const AT_FDCWD: i32 = -100;

    println!("installing filter...");
    seccomp::install().expect("filter install");
    println!("filter installed");

    println!("step: getpid");
    unsafe {
        libc::getpid();
    }

    println!("step: uname");
    unsafe {
        let mut u: libc::utsname = std::mem::zeroed();
        libc::uname(&mut u);
    }

    println!("step: clock_gettime");
    unsafe {
        let mut ts: libc::timespec = std::mem::zeroed();
        libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts);
    }

    println!("step: sysinfo");
    unsafe {
        let mut s: libc::sysinfo = std::mem::zeroed();
        libc::sysinfo(&mut s);
    }

    println!("step: statx (nr=332)");
    unsafe {
        let mut buf = [0u8; 256];
        let path = b"/tmp\0";
        libc::syscall(
            332,
            AT_FDCWD,
            path.as_ptr(),
            0i32,
            0x7ffu32,
            buf.as_mut_ptr(),
        );
    }

    println!("step: newfstatat (nr=262)");
    unsafe {
        let mut buf = [0u8; 144];
        let path = b"/tmp\0";
        libc::syscall(262, AT_FDCWD, path.as_ptr(), buf.as_mut_ptr(), 0i32);
    }

    println!("step: openat/read/close");
    unsafe {
        let path = b"/etc/hostname\0";
        let fd = libc::open(path.as_ptr() as *const _, libc::O_RDONLY);
        assert!(fd >= 0, "open failed");
        let mut b = [0u8; 64];
        libc::read(fd, b.as_mut_ptr() as *mut _, 64);
        libc::close(fd);
    }

    println!("step: getrandom");
    unsafe {
        let mut b = [0u8; 16];
        libc::getrandom(b.as_mut_ptr() as *mut _, 16, 0u32);
    }

    println!("step: sched_getaffinity (nr=204)");
    unsafe {
        let mut set = [0u8; 128];
        libc::syscall(204, 0u32, 128usize, set.as_mut_ptr());
    }

    println!("step: mmap/mincore/munmap");
    unsafe {
        let p = libc::mmap(
            std::ptr::null_mut(),
            4096,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        );
        let mut v: u8 = 0;
        libc::syscall(27, p, 4096usize, &mut v as *mut u8 as *mut libc::c_void);
        libc::munmap(p, 4096);
    }

    println!("step: thread spawn (clone/futex/rseq)");
    {
        let handle = std::thread::spawn(|| println!("hello from thread"));
        handle.join().expect("thread join");
    }

    println!("step: fork + wait4");
    unsafe {
        let pid = libc::fork();
        if pid == 0 {
            libc::_exit(0);
        }
        let mut status = 0i32;
        libc::wait4(pid, &mut status, 0, std::ptr::null_mut());
    }

    println!("step: socketpair(AF_UNIX)");
    unsafe {
        let mut fds = [0i32; 2];
        libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr());
    }

    println!("step: socket(AF_UNIX)");
    unsafe {
        libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0);
    }

    println!("step: readlink(/proc/self/exe)");
    unsafe {
        let mut b = [0u8; 256];
        let path = b"/proc/self/exe\0";
        libc::readlink(path.as_ptr() as *const _, b.as_mut_ptr() as *mut _, 256);
    }

    println!("ALL STEPS PASSED — filter permits every probed category");
}
