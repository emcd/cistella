//! `landlock_kill_wrapper` — deterministic Landlock-unavailable
//! fault injector for the spike's Unsupported assertion.
//!
//! Installs a seccomp-bpf filter that returns `ENOSYS` for
//! `landlock_create_ruleset` (and the other two Landlock syscalls),
//! then `execvp`s the wrapped command. The wrapped process sees
//! Landlock as if the kernel lacked the feature, regardless of host
//! capability, so the spike's typed-Unsupported surface is exercised
//! deterministically on every host.
//!
//! Setup order (per Advisor direction):
//!   1. `prctl(PR_SET_NO_NEW_PRIVS, 1, ...)` — once set, the seccomp
//!      filter below becomes irrevocable for this thread and its
//!      descendants.
//!   2. `seccomp(SECCOMP_SET_MODE_FILTER, 0, &filter)` — install
//!      the BPF filter.
//!   3. `execvp(argv)` — wrapped command inherits the filter.
//!
//! The wrapper does NOT use the Landlock syscalls itself, so the
//! filter is a no-op for setup. After exec, the wrapped process
//! observes `landlock_create_ruleset` returning `ENOSYS` and exits
//! via the helper's typed-Unsupported path.
//!
//! Usage:
//!   landlock_kill_wrapper -- <wrapped-argv>...
//!
//! Exit codes:
//!   0   wrapped command exited normally (rare under filter)
//!   1   setup failed (prctl, seccomp, or filter install)
//!   2   execvp failed after filter install (helper in restricted state)
//!   3   bad invocation (no `--`)

use std::ffi::CString;
use std::io;
use std::process::ExitCode;

use libc::{
    ENOSYS, PR_SET_NO_NEW_PRIVS, SECCOMP_RET_ALLOW, SECCOMP_RET_ERRNO, SECCOMP_RET_KILL_THREAD,
    SECCOMP_SET_MODE_FILTER, SYS_prctl, SYS_seccomp, c_long,
};

/// `seccomp(2)` filter mode 1 = install a BPF filter.
const SECCOMP_SET_MODE_FILTER_VALUE: c_long = SECCOMP_SET_MODE_FILTER as c_long;

/// One BPF instruction (matches the kernel's `struct sock_filter`).
#[repr(C)]
#[derive(Clone, Copy)]
struct SockFilter {
    code: u16,
    jt: u8,
    jf: u8,
    k: u32,
}

const BPF_LD: u16 = 0x00;
const BPF_W: u16 = 0x00;
const BPF_ABS: u16 = 0x20;
const BPF_JMP: u16 = 0x05;
const BPF_JEQ: u16 = 0x10;
const BPF_RET: u16 = 0x06;

const fn bpf_stmt(code: u16, k: u32) -> SockFilter {
    SockFilter {
        code,
        jt: 0,
        jf: 0,
        k,
    }
}

const fn bpf_jump(code: u16, k: u32, jt: u8, jf: u8) -> SockFilter {
    SockFilter { code, jt, jf, k }
}

/// `AUDIT_ARCH_AARCH64` = `EM_AARCH64 | __AUDIT_ARCH_64BIT | __AUDIT_ARCH_LE`
/// = 0xC00000B7 (matches the kernel's `linux/audit.h` for aarch64).
///
/// For portability, the filter currently hard-codes the aarch64
/// arch because that's what runs the spike today. Adding x86_64
/// (0xC000003E) is a one-line change when needed.
const AUDIT_ARCH_AARCH64: u32 = 0xC00000B7;

/// Landlock syscall numbers — aarch64 (this host). x86_64 is
/// 444/445/446, s390x 436/437/438, etc. The numbers below match the
/// kernel headers for aarch64.
const SYSCALL_LANDLOCK_CREATE_RULESET_AARCH64: u32 = 444;
const SYSCALL_LANDLOCK_ADD_RULE_AARCH64: u32 = 445;
const SYSCALL_LANDLOCK_RESTRICT_SELF_AARCH64: u32 = 446;

/// BPF filter: deny the three Landlock syscalls with `ENOSYS`,
/// allow everything else. 11 instructions.
fn landlock_filter() -> [SockFilter; 11] {
    let errno_enosys = SECCOMP_RET_ERRNO | (ENOSYS as u32);
    [
        // Load `seccomp_data.arch`.
        bpf_stmt(BPF_LD | BPF_W | BPF_ABS, 4),
        // If arch == AUDIT_ARCH_AARCH64, skip 1 (continue).
        bpf_jump(BPF_JMP | BPF_JEQ, AUDIT_ARCH_AARCH64, 1, 0),
        // Wrong arch: kill thread (this spike is single-architecture).
        bpf_stmt(BPF_RET, SECCOMP_RET_KILL_THREAD),
        // Load `seccomp_data.nr`.
        bpf_stmt(BPF_LD | BPF_W | BPF_ABS, 0),
        // If nr == landlock_create_ruleset, skip 1 (return ENOSYS).
        bpf_jump(
            BPF_JMP | BPF_JEQ,
            SYSCALL_LANDLOCK_CREATE_RULESET_AARCH64,
            0,
            1,
        ),
        bpf_stmt(BPF_RET, errno_enosys),
        // If nr == landlock_add_rule, skip 1 (return ENOSYS).
        bpf_jump(BPF_JMP | BPF_JEQ, SYSCALL_LANDLOCK_ADD_RULE_AARCH64, 0, 1),
        bpf_stmt(BPF_RET, errno_enosys),
        // If nr == landlock_restrict_self, skip 1 (return ENOSYS).
        bpf_jump(
            BPF_JMP | BPF_JEQ,
            SYSCALL_LANDLOCK_RESTRICT_SELF_AARCH64,
            0,
            1,
        ),
        bpf_stmt(BPF_RET, errno_enosys),
        // Allow all other syscalls.
        bpf_stmt(BPF_RET, SECCOMP_RET_ALLOW),
    ]
}

/// `prctl(2)` raw syscall wrapper.
unsafe fn prctl_no_new_privs() -> Result<(), i32> {
    let r = unsafe {
        libc::syscall(
            SYS_prctl,
            PR_SET_NO_NEW_PRIVS as c_long,
            1 as c_long,
            0 as c_long,
            0 as c_long,
            0 as c_long,
        )
    };
    if r < 0 {
        Err(io::Error::last_os_error().raw_os_error().unwrap_or(1))
    } else {
        Ok(())
    }
}

/// `seccomp(SECCOMP_SET_MODE_FILTER, 0, &filter)` raw syscall wrapper.
unsafe fn seccomp_install_filter(filter: &[SockFilter]) -> Result<(), i32> {
    #[repr(C)]
    struct SockFprog {
        len: u16,
        filter: *const SockFilter,
    }
    let prog = SockFprog {
        len: filter.len() as u16,
        filter: filter.as_ptr(),
    };
    let r = unsafe {
        libc::syscall(
            SYS_seccomp,
            SECCOMP_SET_MODE_FILTER_VALUE,
            0 as c_long,
            &prog as *const _ as c_long,
        )
    };
    if r < 0 {
        Err(io::Error::last_os_error().raw_os_error().unwrap_or(1))
    } else {
        Ok(())
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let mut split: Option<usize> = None;
    for (i, arg) in args.iter().enumerate().skip(1) {
        if arg == "--" {
            split = Some(i);
            break;
        }
    }
    let Some(split) = split else {
        eprintln!("missing `--` separator");
        return ExitCode::from(3);
    };
    if split + 1 >= args.len() {
        eprintln!("no wrapped command after `--`");
        return ExitCode::from(3);
    }
    let wrapped: &[String] = &args[split + 1..];

    // 1. PR_SET_NO_NEW_PRIVS — must precede the filter install (per
    //    Advisor). After this, the seccomp filter below is
    //    irrevocable for the calling thread and its descendants.
    if let Err(e) = unsafe { prctl_no_new_privs() } {
        eprintln!("landlock_kill_wrapper: prctl(PR_SET_NO_NEW_PRIVS) failed: errno {e}");
        return ExitCode::from(1);
    }

    // 2. Install the BPF filter.
    let filter = landlock_filter();
    if let Err(e) = unsafe { seccomp_install_filter(&filter) } {
        eprintln!("landlock_kill_wrapper: seccomp install failed: errno {e}");
        return ExitCode::from(1);
    }

    // 3. execvp the wrapped command — filter is inherited.
    let c_argv: Vec<CString> = wrapped
        .iter()
        .map(|a| CString::new(a.as_bytes()))
        .collect::<Result<_, _>>()
        .map_err(|e| {
            eprintln!("landlock_kill_wrapper: argv encoding: {e}");
            e
        })
        .unwrap();
    let mut c_argv_ptrs: Vec<*const libc::c_char> = c_argv.iter().map(|s| s.as_ptr()).collect();
    c_argv_ptrs.push(std::ptr::null());
    let c_arg0 = &c_argv[0];
    unsafe {
        libc::execvp(c_arg0.as_ptr(), c_argv_ptrs.as_ptr());
    }
    let e = io::Error::last_os_error().raw_os_error().unwrap_or(1);
    eprintln!("landlock_kill_wrapper: execvp failed: errno {e}");
    ExitCode::from(2)
}
