//! `deny_probe` — exact-syscall denial probe for the 3.4 matrix.
//!
//! Shell redirections cannot produce machine-readable errno
//! (and `: > file` opens `O_WRONLY|O_CREAT|O_TRUNC`, which
//! fails under `WRITE_FILE` without ever testing the ABI-v3
//! `LANDLOCK_ACCESS_FS_TRUNCATE` bypass `open(O_RDONLY|
//! O_TRUNC)`). This helper performs one exact filesystem
//! operation and records `OK` or `ERRNO=<n>` to a result
//! file, exiting 0 either way: the same binary runs
//! unconfined (companion exec proves the operation) and
//! confined (hooked harness proves the denial), so pre/post
//! controls share op, path, uid, and mounts. A result-file
//! write failure exits 2 (loud and distinct from probe
//! outcomes).
//!
//! Cargo autodiscovers (lands at `target/<profile>/examples/`);
//! out of `package.include`. Staged into guests by mount in
//! the hook-live fixture, never shipped.

use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;

fn record(result_path: &str, outcome: &str) -> std::process::ExitCode {
    let body = format!("{outcome}\n");
    match std::fs::write(result_path, body) {
        Ok(()) => std::process::ExitCode::from(0),
        Err(error) => {
            eprintln!("PROBE_FAIL: cannot record outcome: {error}");
            std::process::ExitCode::from(2)
        }
    }
}

fn errno_of(error: std::io::Error) -> String {
    match error.raw_os_error() {
        Some(errno) => format!("ERRNO={errno}"),
        None => "ERRNO=unknown".to_string(),
    }
}

fn usage() -> std::process::ExitCode {
    eprintln!("usage: deny_probe <result> <op> [operands...]");
    eprintln!(
        "ops: open-wronly <path> | open-ro-trunc <path> | create-excl <path> | unlink <path> | rename <src> <dst>"
    );
    std::process::ExitCode::from(2)
}

fn main() -> std::process::ExitCode {
    let mut argv = std::env::args();
    let _program = argv.next();
    let (Some(result_path), Some(op)) = (argv.next(), argv.next()) else {
        return usage();
    };
    let outcome = match op.as_str() {
        // Plain write-only open of a pre-existing file (no
        // create, no truncate flags): denied at open exactly
        // by LANDLOCK_ACCESS_FS_WRITE_FILE. The path must
        // exist — a missing path reports ENOENT instead.
        "open-wronly" => {
            let Some(path) = argv.next() else {
                return usage();
            };
            match std::fs::OpenOptions::new().write(true).open(&path) {
                Ok(mut file) => match file.write_all(b"x") {
                    Ok(()) => "OK".to_string(),
                    Err(error) => errno_of(error),
                },
                Err(error) => errno_of(error),
            }
        }
        // The ABI-v3 shape: read-only open with truncate.
        // Denied only by LANDLOCK_ACCESS_FS_TRUNCATE.
        "open-ro-trunc" => {
            let Some(path) = argv.next() else {
                return usage();
            };
            match std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(nix::libc::O_TRUNC)
                .open(&path)
            {
                Ok(_) => "OK".to_string(),
                Err(error) => errno_of(error),
            }
        }
        // Creation of a fresh name.
        "create-excl" => {
            let Some(path) = argv.next() else {
                return usage();
            };
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(_) => "OK".to_string(),
                Err(error) => errno_of(error),
            }
        }
        "unlink" => {
            let Some(path) = argv.next() else {
                return usage();
            };
            match std::fs::remove_file(&path) {
                Ok(()) => "OK".to_string(),
                Err(error) => errno_of(error),
            }
        }
        "rename" => {
            let (Some(source), Some(target)) = (argv.next(), argv.next()) else {
                return usage();
            };
            match std::fs::rename(&source, &target) {
                Ok(()) => "OK".to_string(),
                Err(error) => errno_of(error),
            }
        }
        _ => return usage(),
    };
    record(&result_path, &outcome)
}
