//! Landlock helper: digest-pinned-blob that applies a Landlock ruleset
//! and `execvp`s the wrapped command.
//!
//! Task 3.2 (Landlock spike, `framework-isolators-extensions`). The
//! extension guest-hook wire shape (per `extension-protocol` spec)
//! carries `{artifact, staging, order, argv_prefix, probe,
//! on_failure}`. This helper is the runtime that `argv_prefix`
//! resolves to in the Landlock case: it accepts `--allow=<path>`
//! (repeatable), builds a Landlock ABI v1 ruleset with read+execute
//! access to each listed path, applies it via `landlock_restrict_self`,
//! and execs the wrapped argv.
//!
//! Exec ancestry: the helper `execvp`s the wrapped command, so the
//! wrapped process inherits the helper's PID. The helper is the
//! direct ancestor of the harness (no intermediate `fork` after
//! `restrict_self`), which is the contract the protocol assumes.
//!
//! Live-tier gated: this binary is used by `tests/integration/
//! landlock_spike.rs` to prove the three-way assertions
//! (`admitted` / `EACCES` / typed pre-execute `Unsupported`).
//! `cargo test --test integration` builds it; the live tests are
//! `#[ignore]` and require an operator-authorized seat (Landlock
//! restrict_self needs CAP_SYS_ADMIN-equivalent or unfiltered
//! seccomp; the dogfood container seccomp-filter blocks it — see
//! the spike report).
//!
//! Self-contained in `examples/`: Cargo autodiscovers, lands at
//! `target/<profile>/examples/landlock_helper`, stays out of
//! `package.include`.
//!
//! Usage:
//!   landlock_helper --allow=/tmp --allow=/proc/self/fd -- <argv>...
//!
//! Exit codes:
//!   0 — wrapped command exited normally
//!   1 — Landlock ruleset construction or application failed
//!   2 — `execvp` failed after Landlock was applied (helper could
//!        not transition to the wrapped command)
//!   3 — Bad invocation (no `--`, unknown flag, etc.)

use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::process::ExitCode;

use libc::{
    EACCES, EBADF, ENOSYS, EPERM, O_CLOEXEC, O_PATH, PR_SET_NO_NEW_PRIVS, SYS_landlock_add_rule,
    SYS_landlock_create_ruleset, SYS_landlock_restrict_self, c_int, close, fcntl, open,
};

// (The `LANDLOCK_ABI_VERSION` constant was removed — the helper
// always uses ABI v1 implicitly via the `landlock_ruleset_attr`
// shape. When v2 (refer/truncate) lands, replace the v1 struct with
// the v2 layout and gate by probe `op`.)

/// Access rights we exercise in the spike. Read+execute cover the
/// `/proc/self/fd`, `/tmp`, and `bin/` paths the wrapped command
/// needs to start; full fs subset (excluding refer/truncate which
/// need ABI v2) keeps the test surface kernel-version-tolerant.
const HANDLED_ACCESS_FS: u64 = (1 << 0) // EXECUTE
    | (1 << 1) // WRITE_FILE
    | (1 << 2) // READ_FILE
    | (1 << 3) // READ_DIR
    | (1 << 4) // REMOVE_DIR
    | (1 << 5) // REMOVE_FILE
    | (1 << 6) // MAKE_CHAR
    | (1 << 7) // MAKE_DIR
    | (1 << 8) // MAKE_REG
    | (1 << 9) // MAKE_SOCK
    | (1 << 10) // MAKE_FIFO
    | (1 << 11) // MAKE_BLOCK
    | (1 << 12); // MAKE_SYM

/// `landlock_create_ruleset` ABI attributes (kernel struct
/// `landlock_ruleset_attr`). v1 only supports `handled_access_fs`;
/// the `scoped` and `handled_access_net` fields are reserved and
/// must be zero.
#[repr(C)]
struct LandlockRulesetAttr {
    handled_access_fs: u64,
    handled_access_net: u64,
    scoped: u64,
}

/// `landlock_add_rule` attribute for `LANDLOCK_RULE_PATH_BENEATH`
/// (kernel struct `landlock_path_beneath_attr`). v1 shape.
#[repr(C)]
#[derive(Clone, Copy)]
struct LandlockPathBeneathAttr {
    allowed_access: u64,
    parent_fd: c_int,
    path_fd: u64, // reserved, must be zero (v1)
}

const LANDLOCK_RULE_PATH_BENEATH: u64 = 1;

/// Thin syscall wrapper (libc exposes `SYS_landlock_*` numbers but
/// not the typed helpers; we use raw `syscall()`).
unsafe fn syscall2(num: libc::c_long, a1: libc::c_long, a2: libc::c_long) -> c_int {
    unsafe { libc::syscall(num, a1, a2) as c_int }
}

unsafe fn syscall4(
    num: libc::c_long,
    a1: libc::c_long,
    a2: libc::c_long,
    a3: libc::c_long,
    a4: libc::c_long,
) -> c_int {
    unsafe { libc::syscall(num, a1, a2, a3, a4) as c_int }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    // Parse --allow=<path> until `--`.
    let mut allows: Vec<PathBuf> = Vec::new();
    let mut argv_split: Option<usize> = None;
    for (i, arg) in args.iter().enumerate().skip(1) {
        if arg == "--" {
            argv_split = Some(i);
            break;
        }
        if let Some(path) = arg.strip_prefix("--allow=") {
            allows.push(PathBuf::from(path));
        } else if arg == "--help" || arg == "-h" {
            print_help();
            return ExitCode::SUCCESS;
        } else {
            eprintln!("unknown argument: {arg}");
            return ExitCode::from(3);
        }
    }
    let Some(split) = argv_split else {
        eprintln!("missing `--` separator");
        return ExitCode::from(3);
    };
    if split + 1 >= args.len() {
        eprintln!("no wrapped command after `--`");
        return ExitCode::from(3);
    }
    let wrapped_argv: &[String] = &args[split + 1..];

    if let Err(error) = apply_landlock_and_exec(&allows, wrapped_argv) {
        // Translate internal errors into exit codes the spike
        // assertions can classify deterministically.
        eprintln!("landlock_helper: {error}");
        return ExitCode::from(error.exit_code());
    }
    unreachable!("apply_landlock_and_exec either execs or returns an error")
}

/// Built-in error type so the exit-code mapping stays in one place.
#[derive(Debug)]
enum HelperError {
    /// `landlock_create_ruleset` returned -1 OR `landlock_restrict_self`
    /// refused to apply. The payload names the observed syscall and
    /// errno only (e.g. `landlock_create_ruleset: ENOSYS`): ENOSYS is
    /// also what seccomp returns when a filter blocks the syscall, so
    /// no physical cause is inferred here. Both surface as typed
    /// pre-execute `Unsupported` at the host dispatch layer.
    Unsupported(&'static str),
    /// `landlock_add_rule` failed: an allowed path could not be
    /// opened. Distinct from `Unsupported` (kernel feature is
    /// present) — this is a precondition failure.
    BadPath(PathBuf, i32),
    /// `execvp` failed after Landlock was applied. The helper is
    /// stuck in a restricted state and cannot continue; the host
    /// should treat this as a hard failure.
    ExecFailed(i32),
    /// Argument construction failed (rare — usually only on
    /// interior NUL bytes).
    BadArgument(String),
}

impl HelperError {
    fn exit_code(&self) -> u8 {
        match self {
            Self::Unsupported(_) => 1,
            Self::BadPath(_, _) => 1,
            Self::ExecFailed(_) => 2,
            Self::BadArgument(_) => 3,
        }
    }
}

impl std::fmt::Display for HelperError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported(why) => write!(f, "unsupported: {why}"),
            Self::BadPath(p, e) => write!(f, "bad path {}: errno {}", p.display(), e),
            Self::ExecFailed(e) => write!(f, "execvp failed: errno {e}"),
            Self::BadArgument(s) => write!(f, "bad argument: {s}"),
        }
    }
}

fn apply_landlock_and_exec(allows: &[PathBuf], argv: &[String]) -> Result<(), HelperError> {
    // Open each allowed path with O_PATH so we can pin them by fd
    // for `LANDLOCK_RULE_PATH_BENEATH`. The fd survives in the
    // wrapped process (helper execs into it without CLOEXEC), so the
    // ruleset reaches the wrapped command.
    let mut allowed_fds: Vec<c_int> = Vec::with_capacity(allows.len());
    for path in allows {
        let cpath = CString::new(path.as_os_str().as_bytes())
            .map_err(|e| HelperError::BadArgument(format!("{}: {}", path.display(), e)))?;
        let fd = unsafe { open(cpath.as_ptr(), O_PATH | O_CLOEXEC) };
        if fd < 0 {
            let e = io::Error::last_os_error().raw_os_error().unwrap_or(EBADF);
            // Close any fds we already opened before returning.
            for &f in &allowed_fds {
                unsafe {
                    close(f);
                }
            }
            return Err(HelperError::BadPath(path.clone(), e));
        }
        // We want these fds to survive `execvp` so the kernel
        // resolves `parent_fd` through them in the wrapped process.
        // Drop CLOEXEC so they persist past exec.
        let flags = unsafe { fcntl(fd, libc::F_GETFD) };
        if flags >= 0 {
            unsafe {
                fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
            }
        }
        allowed_fds.push(fd);
    }

    // Construct the ruleset (v1 ABI).
    let attr = LandlockRulesetAttr {
        handled_access_fs: HANDLED_ACCESS_FS,
        handled_access_net: 0,
        scoped: 0,
    };
    let ruleset_fd = unsafe {
        syscall4(
            SYS_landlock_create_ruleset,
            &attr as *const _ as libc::c_long,
            std::mem::size_of::<LandlockRulesetAttr>() as libc::c_long,
            0,
            0,
        )
    };
    if ruleset_fd < 0 {
        let e = io::Error::last_os_error().raw_os_error().unwrap_or(EPERM);
        for &f in &allowed_fds {
            unsafe {
                close(f);
            }
        }
        // Report only what the syscall returned — do NOT infer a
        // physical cause (seccomp-blocked vs kernel-absent vs
        // capability-denied are indistinguishable from errno alone).
        // The host dispatch layer can probe if it wants a richer
        // verdict; the helper stays truthful.
        return Err(match e {
            ENOSYS => HelperError::Unsupported("landlock_create_ruleset: ENOSYS"),
            EPERM => HelperError::Unsupported("landlock_create_ruleset: EPERM"),
            _ => HelperError::Unsupported("landlock_create_ruleset: refused"),
        });
    }

    // Add each allowed path as `LANDLOCK_RULE_PATH_BENEATH` with
    // the full access subset (no separate read/write — helper is
    // exercising the typed-spike surface; finer access masks can be
    // added once the host wire shape lands).
    let rule_attr = LandlockPathBeneathAttr {
        allowed_access: HANDLED_ACCESS_FS,
        parent_fd: 0,
        path_fd: 0,
    };
    for &fd in &allowed_fds {
        let mut rule = rule_attr;
        rule.parent_fd = fd;
        let r = unsafe {
            syscall4(
                SYS_landlock_add_rule,
                ruleset_fd as libc::c_long,
                LANDLOCK_RULE_PATH_BENEATH as libc::c_long,
                &rule as *const _ as libc::c_long,
                0,
            )
        };
        if r < 0 {
            let e = io::Error::last_os_error().raw_os_error().unwrap_or(EBADF);
            for &f in &allowed_fds {
                unsafe {
                    close(f);
                }
            }
            unsafe {
                close(ruleset_fd);
            }
            return Err(HelperError::BadPath(format!("fd={fd}").into(), e));
        }
    }

    // `landlock_restrict_self(2)` accepts either CAP_SYS_ADMIN in the
    // calling user namespace OR `PR_SET_NO_NEW_PRIVS=1` on the calling
    // thread. The seat user has no effective capabilities, so we set
    // `PR_SET_NO_NEW_PRIVS` to take the second branch. Without this,
    // a bare EPERM from `restrict_self` proves nothing about Landlock
    // support — it proves the helper never set `no_new_privs`.
    //
    // `prctl(PR_SET_NO_NEW_PRIVS, ...)` is irreversible for the calling
    // thread; setting it is safe here because the helper is a one-shot
    // Landlock wrapper and the ruleset is the next syscall anyway.
    let pr = unsafe {
        libc::syscall(
            libc::SYS_prctl,
            PR_SET_NO_NEW_PRIVS as libc::c_long,
            1 as libc::c_long,
            0,
            0,
            0,
        )
    };
    if pr < 0 {
        let e = io::Error::last_os_error().raw_os_error().unwrap_or(EPERM);
        for &f in &allowed_fds {
            unsafe {
                close(f);
            }
        }
        unsafe {
            close(ruleset_fd);
        }
        return Err(match e {
            ENOSYS => HelperError::Unsupported("prctl(PR_SET_NO_NEW_PRIVS): ENOSYS"),
            EPERM => HelperError::Unsupported("prctl(PR_SET_NO_NEW_PRIVS): EPERM"),
            _ => HelperError::Unsupported("prctl(PR_SET_NO_NEW_PRIVS): refused"),
        });
    }

    // Apply the ruleset. `landlock_restrict_self` is the gate that
    // turns the ruleset into an enforced restriction on this thread.
    // The helper reports only what the syscall returned — physical
    // cause (seccomp-blocked vs kernel-absent vs capability-denied)
    // is not inferable from errno alone; the host dispatch layer
    // probes if it needs a richer verdict.
    let r = unsafe { syscall2(SYS_landlock_restrict_self, ruleset_fd as libc::c_long, 0) };
    if r < 0 {
        let e = io::Error::last_os_error().raw_os_error().unwrap_or(EPERM);
        for &f in &allowed_fds {
            unsafe {
                close(f);
            }
        }
        unsafe {
            close(ruleset_fd);
        }
        return Err(match e {
            ENOSYS => HelperError::Unsupported("landlock_restrict_self: ENOSYS"),
            EPERM => HelperError::Unsupported("landlock_restrict_self: EPERM"),
            _ => HelperError::Unsupported("landlock_restrict_self: refused"),
        });
    }
    // ruleset_fd is consumed by restrict_self — close not required.
    // Allowed fds must stay open (drop CLOEXEC'd them above) so the
    // wrapped process can resolve them.
    //
    // EXECVP — Landlock is enforced on this thread and inherits
    // through exec into the wrapped process. From this point on, the
    // helper's PID becomes the wrapped process's PID; exec ancestry
    // is the helper → wrapped (no fork), per the protocol's contract.
    let c_argv: Vec<CString> = argv
        .iter()
        .map(|a| CString::new(a.as_bytes()))
        .collect::<Result<_, _>>()
        .map_err(|e| HelperError::BadArgument(format!("argv: {e}")))?;
    let mut c_argv_ptrs: Vec<*const libc::c_char> = c_argv.iter().map(|s| s.as_ptr()).collect();
    c_argv_ptrs.push(std::ptr::null());
    let c_arg0 = &c_argv[0];
    unsafe {
        libc::execvp(c_arg0.as_ptr(), c_argv_ptrs.as_ptr());
    }
    // execvp only returns on error.
    let e = io::Error::last_os_error().raw_os_error().unwrap_or(EACCES);
    // Note: we cannot close the allowed_fds here — the helper is in
    // a Landlock-restricted state and might not be able to close
    // arbitrary fds. The kernel cleans them up at process exit.
    Err(HelperError::ExecFailed(e))
}

#[allow(dead_code)]
fn print_help() {
    eprintln!("landlock_helper — Landlock ABI v1 wrapper helper");
    eprintln!();
    eprintln!("Usage:");
    eprintln!("  landlock_helper --allow=<path> [--allow=<path>...] -- <argv>...");
    eprintln!();
    eprintln!("Exit codes:");
    eprintln!("  0  wrapped command exited normally");
    eprintln!("  1  Landlock unsupported or ruleset construction failed");
    eprintln!("  2  execvp failed after Landlock was applied");
    eprintln!("  3  bad invocation");
}
