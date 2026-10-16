//! Transport integration: forkpty DA1/0x03, isatty/stty size, exit 42, 1 MB sha256.
//!
//! Invoked via `cargo nextest` and kept alongside `src/bin/terminal-spike.rs`
//! as the manual regression command.

use std::process::Command;

use super::helpers::*;
use nix::pty::{ForkptyResult, forkpty};
use nix::sys::wait::{self, WaitStatus};

fn podman_available() -> bool {
    let mut cmd = Command::new("podman");
    cmd.args(["info", "--format", "{{.Host.Security.Rootless}}"]);
    run_bounded(cmd, RUN_CISTELLA_TIMEOUT).is_ok_and(|o| o.status.success())
}

fn image_ref() -> String {
    std::env::var("CISTELLA_TEST_IMAGE")
        .unwrap_or_else(|_| "localhost/cistella/opencode:example".to_string())
}

/// Bounded reap of a forkpty child: WNOHANG loop for a grace
/// window, then SIGKILL + bounded pid reap. Never a blocking
/// waitpid. A waitpid error (e.g. already reaped) ends the loop —
/// there is nothing left to reap.
fn reap_pty_child(child: nix::unistd::Pid) {
    let start = std::time::Instant::now();
    let grace = std::time::Duration::from_secs(5);
    while wait::waitpid(child, Some(wait::WaitPidFlag::WNOHANG))
        .is_ok_and(|s| s == WaitStatus::StillAlive)
    {
        if start.elapsed() > grace {
            let _ = nix::sys::signal::kill(child, nix::sys::signal::Signal::SIGKILL);
            let _ = reap_bounded_pid(child, std::time::Duration::from_secs(10));
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn transport_harness() {
    if !podman_available() {
        eprintln!("skip: podman not available");
        return;
    }
    let image = image_ref();
    // Ensure image exists; try building example if missing.
    let mut probe = Command::new("podman");
    probe.args(["image", "exists", &image]);
    let has_image = run_bounded(probe, RUN_CISTELLA_TIMEOUT).is_ok_and(|o| o.status.success());
    if !has_image {
        eprintln!("skip: image {image} not present (build via data/dockerfiles/validate.sh)");
        return;
    }

    let cname = format!("cistella-it-{}-{}", std::process::id(), rand_suffix());

    // Start detached container with keep-id and closed env, no TERMINFO, baked terminfo
    let term = std::env::var("TERM").unwrap_or_else(|_| "xterm-ghostty".to_string());
    let mut run = Command::new("podman");
    run.args([
        "run",
        "--detach",
        "--rm",
        "--userns=keep-id",
        "--name",
        &cname,
        "-e",
        &format!("TERM={term}"),
        "--",
        &image,
        "sleep",
        "300",
    ]);
    if let Ok(ct) = std::env::var("COLORTERM")
        && !ct.is_empty()
    {
        run.args(["-e", &format!("COLORTERM={ct}")]);
    }
    // Rebuild with proper args order: podman run -d ... -e TERM -- image sleep
    // Do fresh:
    let mut cmd = Command::new("podman");
    cmd.args([
        "run",
        "--detach",
        "--rm",
        "--userns=keep-id",
        "--name",
        &cname,
        "-e",
        &format!("TERM={term}"),
        &image,
        "sleep",
        "300",
    ]);
    let out = run_bounded(cmd, RUN_CISTELLA_TIMEOUT).expect("podman run");
    assert!(
        out.status.success(),
        "podman run failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let mut ok = true;

    ok &= check_isatty(&cname);
    ok &= check_exit42(&cname);
    ok &= check_da1_via_forkpty(&cname);
    ok &= check_sigint_via_forkpty(&cname);
    ok &= check_large_payload(&cname);
    ok &= check_terminfo(&cname);

    let mut stop = Command::new("podman");
    stop.args(["stop", "--time", "2", &cname]);
    let _ = run_bounded(stop, RUN_CISTELLA_TIMEOUT);

    assert!(ok, "transport harness failed; see output above");
}

fn rand_suffix() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{n:x}")
}

fn check_isatty(cname: &str) -> bool {
    // Piped exec should be isatty:false / 0 0; forkpty exec -i -t should be true
    let mut cmd = Command::new("podman");
    cmd.args([
        "exec",
        cname,
        "sh",
        "-c",
        "if [ -t 1 ]; then echo true; else echo false; fi; stty size 2>&1 || echo 0 0",
    ]);
    let piped = run_bounded(cmd, RUN_CISTELLA_TIMEOUT).expect("exec");
    let txt = String::from_utf8_lossy(&piped.stdout);
    assert!(
        txt.contains("false"),
        "piped exec expected false, got {txt}"
    );
    assert!(txt.contains("0 0"), "piped stty expected 0 0, got {txt}");

    // forkpty
    match unsafe { forkpty(None, None) } {
        Ok(ForkptyResult::Parent { child, master }) => {
            use std::io::Read;
            let mut f = std::fs::File::from(master);
            let mut buf = [0u8; 4096];
            let start = std::time::Instant::now();
            let mut acc = Vec::new();
            while start.elapsed() < std::time::Duration::from_secs(5) {
                match f.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        acc.extend_from_slice(&buf[..n]);
                        if String::from_utf8_lossy(&acc).contains("isatty:true") {
                            break;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                    Err(_) => break,
                }
                if wait::waitpid(child, Some(wait::WaitPidFlag::WNOHANG))
                    .is_ok_and(|s| s != WaitStatus::StillAlive)
                {
                    break;
                }
            }
            // wait child
            let _ = wait::waitpid(child, None);
            let txt = String::from_utf8_lossy(&acc).to_string();
            eprintln!("isatty forkpty acc: {txt}");
            txt.contains("isatty:true")
        }
        Ok(ForkptyResult::Child) => {
            use std::os::unix::process::CommandExt;
            let e = Command::new("podman")
                .args([
                    "exec", "-i", "-t", cname, "sh", "-c",
                    "if [ -t 1 ]; then echo isatty:true; else echo isatty:false; fi; stty size; echo DONE",
                ])
                .exec();
            eprintln!("exec failed: {e:?}");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("forkpty failed: {e}");
            false
        }
    }
}

fn check_exit42(cname: &str) -> bool {
    let mut cmd = Command::new("podman");
    cmd.args(["exec", cname, "sh", "-c", "exit 42"]);
    let out = run_bounded(cmd, RUN_CISTELLA_TIMEOUT).expect("exit42");
    let code = out.status.code().unwrap_or(-1);
    eprintln!("exit42 code {code}");
    code == 42
}

fn check_terminfo(cname: &str) -> bool {
    let mut cmd = Command::new("podman");
    cmd.args([
        "exec",
        cname,
        "sh",
        "-c",
        "infocmp xterm-ghostty >/dev/null && [ \"$(tput colors)\" = \"256\" ] && echo ok",
    ]);
    let out = run_bounded(cmd, RUN_CISTELLA_TIMEOUT).expect("terminfo");
    let txt = String::from_utf8_lossy(&out.stdout);
    eprintln!("terminfo: {txt} {:?}", out.status);
    out.status.success() && txt.contains("ok")
}

fn check_da1_via_forkpty(cname: &str) -> bool {
    match unsafe { forkpty(None, None) } {
        Ok(ForkptyResult::Parent { child, master }) => {
            use std::io::{Read, Write};
            use std::os::unix::io::AsRawFd;
            let mut f = std::fs::File::from(master);
            // Nonblocking master: the WouldBlock arm below only
            // bounds the window if the fd is actually nonblocking
            // (forkpty hands back a blocking master).
            if let Ok(flags) = nix::fcntl::fcntl(f.as_raw_fd(), nix::fcntl::FcntlArg::F_GETFL) {
                let mut oflags = nix::fcntl::OFlag::from_bits_retain(flags);
                oflags.insert(nix::fcntl::OFlag::O_NONBLOCK);
                if nix::fcntl::fcntl(f.as_raw_fd(), nix::fcntl::FcntlArg::F_SETFL(oflags)).is_err()
                {
                    eprintln!("DA1 pty: O_NONBLOCK setup refused");
                    return false;
                }
            } else {
                eprintln!("DA1 pty: F_GETFL refused");
                return false;
            }
            let mut buf = vec![0u8; 4096];
            let mut acc = Vec::new();
            let start = std::time::Instant::now();
            let mut saw = false;
            while start.elapsed() < std::time::Duration::from_secs(6) {
                let n = match f.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(50));
                        continue;
                    }
                    Err(_) => break,
                };
                acc.extend_from_slice(&buf[..n]);
                if acc.windows(3).any(|w| w == [0x1b, b'[', b'c']) {
                    saw = true;
                    break;
                }
                if wait::waitpid(child, Some(wait::WaitPidFlag::WNOHANG))
                    .is_ok_and(|s| s != WaitStatus::StillAlive)
                {
                    break;
                }
            }
            if saw {
                let _ = f.write_all(b"\x1b[?6c");
                let _ = f.flush();
                eprintln!("DA1 saw query, wrote reply");
            } else {
                eprintln!("DA1 no query seen len {}", acc.len());
            }
            // Bounded reap of the forkpty child (DONE seen; it is
            // exiting): WNOHANG loop, SIGKILL past the bound, never
            // a blocking waitpid.
            reap_pty_child(child);
            let mut cat = Command::new("podman");
            cat.args(["exec", cname, "cat", "/tmp/da1_harness"]);
            let check = run_bounded(cat, RUN_CISTELLA_TIMEOUT);
            let txt = check
                .as_ref()
                .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
                .unwrap_or_default();
            let mut rm = Command::new("podman");
            rm.args(["exec", cname, "rm", "-f", "/tmp/da1_harness"]);
            let _ = run_bounded(rm, RUN_CISTELLA_TIMEOUT);
            eprintln!("DA1 container reply {txt:?} saw {saw}");
            saw && (txt.contains("?6") || txt.contains("6c"))
        }
        Ok(ForkptyResult::Child) => {
            use std::os::unix::process::CommandExt;
            let e = Command::new("podman")
                .args([
                    "exec", "-i", "-t", cname, "bash", "-c",
                    "stty raw -echo; printf \"\\033[c\"; IFS= read -t 5 -r -d c reply; printf \"DA1:%s c\" \"$reply\" > /tmp/da1_harness; cat /tmp/da1_harness; echo DONE",
                ])
                .exec();
            eprintln!("exec {e:?}");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("forkpty {e}");
            false
        }
    }
}

fn check_sigint_via_forkpty(cname: &str) -> bool {
    match unsafe { forkpty(None, None) } {
        Ok(ForkptyResult::Parent { child, master }) => {
            use std::io::{Read, Write};
            use std::os::unix::io::AsRawFd;
            let mut f = std::fs::File::from(master);
            // Nonblocking master: without this the `read` below can
            // sit past the READY window on a silent child.
            if let Ok(flags) = nix::fcntl::fcntl(f.as_raw_fd(), nix::fcntl::FcntlArg::F_GETFL) {
                let mut oflags = nix::fcntl::OFlag::from_bits_retain(flags);
                oflags.insert(nix::fcntl::OFlag::O_NONBLOCK);
                if nix::fcntl::fcntl(f.as_raw_fd(), nix::fcntl::FcntlArg::F_SETFL(oflags)).is_err()
                {
                    eprintln!("sigint pty: O_NONBLOCK setup refused");
                    return false;
                }
            } else {
                eprintln!("sigint pty: F_GETFL refused");
                return false;
            }
            let mut buf = vec![0u8; 4096];
            let mut acc = Vec::new();
            let start = std::time::Instant::now();
            while start.elapsed() < std::time::Duration::from_secs(3) {
                match f.read(&mut buf) {
                    Ok(n) if n > 0 => {
                        acc.extend_from_slice(&buf[..n]);
                        if String::from_utf8_lossy(&acc).contains("READY") {
                            break;
                        }
                    }
                    Ok(_) => std::thread::sleep(std::time::Duration::from_millis(50)),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                    Err(_) => break,
                }
            }
            let _ = f.write_all(&[0x03]);
            let _ = f.flush();
            eprintln!("SIGINT wrote 0x03");
            // Bounded reap: the INT-trapped child exits promptly;
            // SIGKILL past the bound, never a blocking waitpid.
            reap_pty_child(child);
            std::thread::sleep(std::time::Duration::from_millis(400));
            let mut cmd = Command::new("podman");
            cmd.args([
                "exec",
                cname,
                "sh",
                "-c",
                "test -f /tmp/sigint_harness && echo present || echo absent",
            ]);
            let out = run_bounded(cmd, RUN_CISTELLA_TIMEOUT).expect("check sigint");
            let txt = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let mut rm = Command::new("podman");
            rm.args(["exec", cname, "rm", "-f", "/tmp/sigint_harness"]);
            let _ = run_bounded(rm, RUN_CISTELLA_TIMEOUT);
            eprintln!("sigint marker {txt}");
            txt == "present"
        }
        Ok(ForkptyResult::Child) => {
            use std::os::unix::process::CommandExt;
            let e = Command::new("podman")
                .args([
                    "exec", "-i", "-t", cname, "bash", "-c",
                    "trap \"touch /tmp/sigint_harness; exit 0\" INT; echo READY; while true; do sleep 1; done",
                ])
                .exec();
            eprintln!("exec {e:?}");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("forkpty {e}");
            false
        }
    }
}

fn check_large_payload(cname: &str) -> bool {
    let mut cmd = Command::new("podman");
    cmd.args([
        "exec",
        cname,
        "sh",
        "-c",
        "head -c 1048576 /dev/zero | tr '\\0' 'A' | sha256sum",
    ]);
    let out = run_bounded(cmd, RUN_CISTELLA_TIMEOUT).expect("inside sha");
    let inside = String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_string();
    let mut cmd = Command::new("podman");
    cmd.args([
        "exec",
        cname,
        "sh",
        "-c",
        "head -c 1048576 /dev/zero | tr '\\0' 'A'",
    ]);
    let stream = run_bounded(cmd, RUN_CISTELLA_TIMEOUT).expect("stream");
    if stream.stdout.len() != 1_048_576 {
        eprintln!("stream len {}", stream.stdout.len());
        return false;
    }
    // host sha
    let mut child = Command::new("sha256sum")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("sha256sum");
    {
        use std::io::Write;
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(&stream.stdout);
        }
    }
    let out = wait_bounded(child, CHILD_REAP_TIMEOUT).expect("wait");
    let host = String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_string();
    eprintln!("large inside {inside} host {host}");
    !inside.is_empty() && inside == host
}
