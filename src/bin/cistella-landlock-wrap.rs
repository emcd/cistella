//! Landlock wrapper binary (task 3.2): the in-container exec ancestor.
//!
//! Two modes, no daemon, no config files:
//!
//! - `--probe`: report the kernel Landlock ABI plus the handled
//!   file-rights mask as JSON on stdout, exit 0. The framework
//!   requires ABI ≥ 3 with the full matrix (through TRUNCATE)
//!   before staging anything.
//! - Ancestor (default): `--diagnostics-fd=N --allow-ro=P...
//!   --allow-rw=P... -- CMD...` — set no-new-privs, build the
//!   ruleset (R+X on every `--allow-ro` route, full rights on every
//!   `--allow-rw` route), restrict, write the applied attestation
//!   to `N`, close `N` on exec, and exec `CMD` in the same lineage
//!   (no fork: the harness inherits the restriction). Any failure
//!   reports on `N` (best effort) plus stderr and exits nonzero
//!   WITHOUT execing — a harness never runs unconfined.
//!
//! Exit codes: 0 applied/probed; 1 unsupported kernel or apply
//! failure; 2 post-apply failure (exec failed, attestation
//! unwritable, diagnostics unsealable); 3 usage.
//!
//! Raw `syscall(2)` glue mirrors the spike helper's proven shape
//! (ABI-v1 struct notes there explain the v1/v3 layout choice);
//! the ruleset attr passes exactly 8 bytes (`handled_access_fs`)
//! and each path-beneath rule exactly 12 packed bytes
//! (`allowed_access` u64 + `parent_fd` s32), so older kernels see
//! only the fields they know.

use std::os::fd::{BorrowedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::process::ExitCode;

use cistella::framework::registry::{
    ANCESTOR_RIGHTS, MIN_LANDLOCK_ABI, REQUIRED_HANDLED_FS, SUBTREE_RIGHTS,
};

/// Raw syscall number/argument word.
type LibcLong = nix::libc::c_long;

/// `LANDLOCK_CREATE_RULESET_VERSION` flag: query the ABI, no fd.
const RULESET_VERSION_FLAG: LibcLong = 1;

/// `LANDLOCK_RULE_PATH_BENEATH` rule type.
const RULE_PATH_BENEATH: LibcLong = 1;

/// Probe/apply budget is framework-owned (deadlines); the wrapper
/// never waits unboundedly except inside the exec'd harness.
/// Thin raw syscalls: libc exposes the numbers but not typed
/// helpers (same discipline as the spike helper).
unsafe fn syscall2(num: LibcLong, a1: LibcLong, a2: LibcLong) -> nix::libc::c_int {
    unsafe { nix::libc::syscall(num, a1, a2) as nix::libc::c_int }
}

unsafe fn syscall4(
    num: LibcLong,
    a1: LibcLong,
    a2: LibcLong,
    a3: LibcLong,
    a4: LibcLong,
) -> nix::libc::c_int {
    unsafe { nix::libc::syscall(num, a1, a2, a3, a4) as nix::libc::c_int }
}

/// Names an errno for typed (value-free) diagnostics; unknown
/// errnos render numerically, never inferred.
fn errno_name(errno: i32) -> String {
    use nix::libc::*;
    match errno {
        ENOSYS => "ENOSYS".to_string(),
        EPERM => "EPERM".to_string(),
        EACCES => "EACCES".to_string(),
        ENOENT => "ENOENT".to_string(),
        ENOTDIR => "ENOTDIR".to_string(),
        EINVAL => "EINVAL".to_string(),
        EBADF => "EBADF".to_string(),
        ELOOP => "ELOOP".to_string(),
        ENODEV => "ENODEV".to_string(),
        EOPNOTSUPP => "EOPNOTSUPP".to_string(),
        _ => format!("errno {errno}"),
    }
}

/// Last errno as a raw number.
fn last_errno() -> i32 {
    std::io::Error::last_os_error()
        .raw_os_error()
        .unwrap_or(nix::libc::EIO)
}

/// Queries the highest supported Landlock ABI version.
fn kernel_abi() -> Result<u32, String> {
    // SAFETY: query-only call (null attr, zero size, VERSION
    // flag); returns the ABI version or -1, no fd, no state.
    let abi = unsafe {
        syscall4(
            nix::libc::SYS_landlock_create_ruleset,
            0,
            0,
            RULESET_VERSION_FLAG,
            0,
        )
    };
    if abi < 0 {
        return Err(format!(
            "landlock_create_ruleset: {}",
            errno_name(last_errno())
        ));
    }
    Ok(abi as u32)
}

/// Creates a ruleset handling exactly the demonstration matrix.
fn create_ruleset() -> Result<RawFd, String> {
    let handled: u64 = REQUIRED_HANDLED_FS;
    // SAFETY: 8-byte attr (handled_access_fs only); older kernels
    // zero-fill the rest. Returns an fd or -1.
    let fd = unsafe {
        syscall4(
            nix::libc::SYS_landlock_create_ruleset,
            &handled as *const u64 as LibcLong,
            std::mem::size_of::<u64>() as LibcLong,
            0,
            0,
        )
    };
    if fd < 0 {
        return Err(format!(
            "landlock_create_ruleset: {}",
            errno_name(last_errno())
        ));
    }
    Ok(fd)
}

/// Adds one `path_beneath` rule: `allowed` rights on the directory
/// opened `O_PATH|O_DIRECTORY|O_NOFOLLOW` (symlinked allows refuse
/// with ELOOP — translated topology paths are canonical dirs).
fn add_path_rule(ruleset: RawFd, path: &str, allowed: u64) -> Result<(), String> {
    use std::ffi::CString;
    let cpath = CString::new(path).map_err(|_| format!("bad path bytes: {path}"))?;
    // SAFETY: O_PATH directory open, no I/O; closed below on every
    // path (success closes after the rule copies the reference —
    // the ruleset holds its own).
    let dir = unsafe {
        nix::libc::open(
            cpath.as_ptr(),
            nix::libc::O_PATH
                | nix::libc::O_DIRECTORY
                | nix::libc::O_NOFOLLOW
                | nix::libc::O_CLOEXEC,
        )
    };
    if dir < 0 {
        return Err(format!(
            "open allow path {path}: {}",
            errno_name(last_errno())
        ));
    }
    // Packed 12-byte rule: allowed_access u64 + parent_fd s32 native
    // endian (matches `landlock_path_beneath_attr` packed layout).
    let mut rule = [0u8; 12];
    rule[..8].copy_from_slice(&allowed.to_ne_bytes());
    rule[8..].copy_from_slice(&(dir as i32).to_ne_bytes());
    // SAFETY: 12-byte packed attr; returns 0 or -1.
    let result = unsafe {
        syscall4(
            nix::libc::SYS_landlock_add_rule,
            ruleset as LibcLong,
            RULE_PATH_BENEATH,
            rule.as_ptr() as LibcLong,
            0,
        )
    };
    // SAFETY: `dir` opened above, owned here, closed exactly once
    // on every path.
    unsafe {
        nix::libc::close(dir);
    }
    if result < 0 {
        return Err(format!(
            "landlock_add_rule {path}: {}",
            errno_name(last_errno())
        ));
    }
    Ok(())
}

/// Writes one JSON line to the diagnostics fd with a full-write
/// loop. Returns success only when every byte landed: callers
/// treat a short/failed write as a failed attestation (never
/// exec past it).
fn diagnose(fd: RawFd, line: &str) -> std::io::Result<()> {
    // SAFETY: borrowed raw fd, transient slice, no ownership
    // transfer; the byte count is checked below.
    let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
    let mut written = 0;
    let bytes = line.as_bytes();
    while written < bytes.len() {
        match nix::unistd::write(borrowed, &bytes[written..]) {
            Ok(0) => {
                return Err(std::io::Error::other("diagnostics write closed"));
            }
            Ok(count) => written += count,
            Err(error) => return Err(std::io::Error::from(error)),
        }
    }
    Ok(())
}

/// Reports failure with a typed JSON line on diagnostics (best
/// effort — a broken fd also fails the write, which is itself the
/// signal) plus a stderr line, with the classification exit code.
/// The harness never runs past this: every caller returns without
/// execing.
fn fail(fd: RawFd, error: &str, code: u8) -> ExitCode {
    // serde_json owns escaping: path/error text (profile- and
    // extension-influenced) can carry quotes, newlines, or
    // control bytes without corrupting the frame.
    let line = serde_json::json!({"applied": false, "error": error}).to_string() + "\n";
    let _ = diagnose(fd, &line);
    eprintln!("landlock-wrap: {error}");
    ExitCode::from(code)
}

/// Serves `--probe`: kernel ABI plus handled mask as JSON.
fn serve_probe() -> ExitCode {
    match kernel_abi() {
        Ok(abi) => {
            println!(
                "{{\"abi\":{abi},\"handled_fs_mask\":{}}}",
                REQUIRED_HANDLED_FS
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            println!("{{\"unsupported\":\"{error}\"}}");
            ExitCode::from(1)
        }
    }
}

/// Parsed ancestor invocation.
struct Ancestor {
    diagnostics: RawFd,
    allow_ro: Vec<String>,
    allow_rw: Vec<String>,
    command: Vec<String>,
}

/// Parses ancestor argv (after argv[0]): `--diagnostics-fd=N`,
/// `--allow-ro=P`, `--allow-rw=P`, then `-- CMD...`.
fn parse_ancestor(args: &[String]) -> Result<Ancestor, String> {
    let mut diagnostics: Option<RawFd> = None;
    let mut allow_ro = Vec::new();
    let mut allow_rw = Vec::new();
    let mut position = 0;
    while position < args.len() {
        let arg = &args[position];
        if arg == "--" {
            position += 1;
            break;
        } else if let Some(value) = arg.strip_prefix("--diagnostics-fd=") {
            let fd: RawFd = value
                .parse()
                .map_err(|_| "bad --diagnostics-fd value".to_string())?;
            if fd < 0 {
                return Err("bad --diagnostics-fd value".to_string());
            }
            diagnostics = Some(fd);
        } else if let Some(path) = arg.strip_prefix("--allow-ro=") {
            if path.is_empty() {
                return Err("empty --allow-ro path".to_string());
            }
            allow_ro.push(path.to_string());
        } else if let Some(path) = arg.strip_prefix("--allow-rw=") {
            if path.is_empty() {
                return Err("empty --allow-rw path".to_string());
            }
            allow_rw.push(path.to_string());
        } else if arg == "--help" || arg == "-h" {
            return Err("help".to_string());
        } else {
            return Err(format!("unknown argument: {arg}"));
        }
        position += 1;
    }
    let diagnostics = diagnostics.ok_or_else(|| "missing --diagnostics-fd".to_string())?;
    if position >= args.len() {
        return Err("no wrapped command after `--`".to_string());
    }
    Ok(Ancestor {
        diagnostics,
        allow_ro,
        allow_rw,
        command: args[position..].to_vec(),
    })
}

/// Applies the ruleset and execs the command: no-new-privs, ABI
/// gate, rules, restrict, attest, seal, exec. Any failure reports
/// and exits WITHOUT execing.
fn serve_ancestor(invocation: &Ancestor) -> ExitCode {
    let fd = invocation.diagnostics;
    let abi = match kernel_abi() {
        Ok(abi) => abi,
        Err(error) => return fail(fd, &format!("unsupported: {error}"), 1),
    };
    if abi < MIN_LANDLOCK_ABI {
        return fail(
            fd,
            &format!("unsupported: kernel ABI {abi} below minimum {MIN_LANDLOCK_ABI}"),
            1,
        );
    }
    // SAFETY: one-shot wrapper; no-new-privs is irreversible for
    // this thread and the ruleset is the next syscall anyway (same
    // discipline as the spike helper).
    let pr = unsafe { nix::libc::prctl(nix::libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
    if pr != 0 {
        return fail(
            fd,
            &format!(
                "unsupported: prctl(PR_SET_NO_NEW_PRIVS): {}",
                errno_name(last_errno())
            ),
            1,
        );
    }
    let ruleset = match create_ruleset() {
        Ok(ruleset) => ruleset,
        Err(error) => return fail(fd, &format!("unsupported: {error}"), 1),
    };
    for path in &invocation.allow_ro {
        if let Err(error) = add_path_rule(ruleset, path, ANCESTOR_RIGHTS) {
            close_fd(ruleset);
            return fail(fd, &format!("bad allow path: {error}"), 1);
        }
    }
    for path in &invocation.allow_rw {
        if let Err(error) = add_path_rule(ruleset, path, SUBTREE_RIGHTS) {
            close_fd(ruleset);
            return fail(fd, &format!("bad allow path: {error}"), 1);
        }
    }
    // SAFETY: gate syscall; returns 0 or -1, no fd, no state
    // beyond the calling thread's restriction.
    let restricted = unsafe {
        syscall2(
            nix::libc::SYS_landlock_restrict_self,
            ruleset as LibcLong,
            0,
        )
    };
    // The ruleset fd served its purpose: close before exec so the
    // harness inherits no stray descriptors (the kernel holds its
    // own reference to the enforced ruleset).
    close_fd(ruleset);
    if restricted != 0 {
        return fail(
            fd,
            &format!(
                "unsupported: landlock_restrict_self: {}",
                errno_name(last_errno())
            ),
            1,
        );
    }
    // Attest BEFORE exec: the framework gates session start on
    // this line, and the write is a CHECKED pre-exec condition —
    // an unwritten attestation (broken fd) exits here without
    // execing, never past it. The attestation repeats the ABI and
    // handled mask so the host re-verifies the matrix at this
    // second trust moment (probe ran earlier, in another process
    // context). Then seal the diagnostics fd so the harness
    // never inherits it (close-on-exec yields EOF).
    let attested = serde_json::json!({
        "applied": true,
        "abi": abi,
        "handled_fs_mask": REQUIRED_HANDLED_FS,
    })
    .to_string()
        + "\n";
    if diagnose(fd, &attested).is_err() {
        eprintln!("landlock-wrap: attestation write failed");
        return ExitCode::from(2);
    }
    if nix::fcntl::fcntl(
        fd,
        nix::fcntl::FcntlArg::F_SETFD(nix::fcntl::FdFlag::FD_CLOEXEC),
    )
    .is_err()
    {
        return fail(fd, "diagnostics seal failed", 2);
    }
    // Exec: the harness inherits the restriction in this same
    // lineage (no fork). `exec` returns only the failure, which is
    // a WRAPPER failure (the harness never started), reported
    // typed on the still-open diagnostics fd before exit.
    //
    // Status-separation invariant (design): this function never
    // returns SUCCESS — post-attestation paths exec or exit
    // nonzero. A clean harness exit therefore proves the exec
    // transition (only the harness could produce it). Signal
    // deaths (wrapper SIGKILLed between seal and execve, or
    // harness signalled later) report as session signals —
    // truthful at session level, never fabricated clean outcomes.
    // QA-only fault seam for that window (deterministic pin of
    // the ambiguous case): with the env set, SIGKILL self after
    // attesting instead of execing. Never set in production.
    if std::env::var("CISTELLA_QA_WRAPPER_FAULT").as_deref() == Ok("kill-after-attest") {
        // SAFETY: intentional self-kill for the fault pin only.
        unsafe {
            nix::libc::raise(nix::libc::SIGKILL);
        }
    }
    let error = std::process::Command::new(&invocation.command[0])
        .args(&invocation.command[1..])
        .exec();
    let line = serde_json::json!({"applied": false, "error": format!("exec failed: {error}")})
        .to_string()
        + "\n";
    let _ = diagnose(fd, &line);
    eprintln!("landlock-wrap: exec failed: {error}");
    ExitCode::from(2)
}

/// Closes one owned raw fd (best effort; pre-restriction, so the
/// close succeeds normally).
fn close_fd(fd: RawFd) {
    // SAFETY: fd opened above in this function, owned here,
    // closed exactly once per call.
    unsafe {
        nix::libc::close(fd);
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|arg| arg == "--probe") {
        if args.len() != 1 {
            eprintln!("landlock-wrap: --probe takes no arguments");
            return ExitCode::from(3);
        }
        return serve_probe();
    }
    let invocation = match parse_ancestor(&args) {
        Ok(invocation) => invocation,
        Err(error) => {
            eprintln!("landlock-wrap: {error}");
            return ExitCode::from(3);
        }
    };
    serve_ancestor(&invocation)
}
