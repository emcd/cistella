//! Blocking-synchronization evidence: lock contention and
//! fifo admission/release proved by observation, never by clock.
//!
//! Split from `helpers.rs` at the file-size limit; re-exported
//! there so existing `helpers::*` call sites keep working.

use std::time::{Duration, Instant};

/// A /proc/locks blocked-waiter match: the exact file (dev+ino)
/// with a pending (`->`-marked) FLOCK held by the exact pid.
/// Name resolution happens against the caller's values, so a
/// flock on any *other* lock can never satisfy this predicate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockedWait {
    /// Waiter pid.
    pub pid: u32,
    /// Lock file device and inode.
    pub dev: u64,
    pub ino: u64,
}

/// Pure /proc/locks blocked-waiter rule: the line must carry the
/// pending marker (`->`), name FLOCK, name this pid, and name
/// this exact dev:ino (maj:min hex, ino decimal per
/// proc_locks(5)). An opened-but-uncontended holder line has no
/// marker and never matches — opening without flocking cannot
/// satisfy the observation.
#[must_use]
pub fn lock_waiter_matches(line: &str, waiter: &BlockedWait) -> bool {
    // Blocked-waiter marker first: holder lines lack it.
    let Some((_, rest)) = line.split_once("->") else {
        return false;
    };
    if !rest.contains("FLOCK") {
        return false;
    }
    let fields: Vec<&str> = rest.split_whitespace().collect();
    // Fields after the marker: FLOCK ADVISORY WRITE <pid>
    // <maj>:<min>:<ino> <start> <end>. Pid match first (cheap
    // reject before parsing device numbers).
    let pid_ok = fields
        .iter()
        .any(|field| field.parse::<u32>().ok() == Some(waiter.pid));
    if !pid_ok {
        return false;
    }
    let expected_major = libc_major(waiter.dev);
    let expected_minor = libc_minor(waiter.dev);
    fields.iter().any(|field| {
        // dev:ino triple as maj:min hex plus decimal inode;
        // parsed numerically so kernel zero-padding (e.g.
        // `02` vs `2`) never matters.
        let mut parts = field.split(':');
        match (parts.next(), parts.next(), parts.next(), parts.next()) {
            (Some(maj), Some(min), Some(ino), None) => {
                u32::from_str_radix(maj, 16).ok() == Some(expected_major)
                    && u32::from_str_radix(min, 16).ok() == Some(expected_minor)
                    && ino.parse::<u64>().ok() == Some(waiter.ino)
            }
            _ => false,
        }
    })
}

fn libc_major(dev: u64) -> u32 {
    ::libc::major(dev)
}

fn libc_minor(dev: u64) -> u32 {
    ::libc::minor(dev)
}

#[cfg(test)]
pub(super) fn makedev_for_test(major: u32, minor: u32) -> u64 {
    // SAFETY: pure libc makedev composition for pins.
    ::libc::makedev(major, minor) as u64
}

/// Waits until a child process is observably BLOCKED on the
/// creation-window lock (not merely holding its fd): the
/// /proc/locks blocked-waiter entry must name this child pid
/// AND this exact lock file (dev+ino). An opened-but-
/// uncontended fd never appears there, so bypassing flock
/// cannot satisfy the wait; a descheduled child reports no
/// blocked entry either. Bounded and fail-loud, with the last
/// observed state in the panic for kernel-string drift
/// diagnosis. Metadata is re-read every iteration (the lock
/// file may be created between polls).
pub fn wait_lock_blocked(child: &mut std::process::Child, timeout: Duration) {
    use std::os::unix::fs::MetadataExt as _;
    let pid = child.id();
    let deadline = Instant::now() + timeout;
    let mut last_seen = "no /proc/locks read yet".to_string();
    loop {
        let lock_meta = std::fs::metadata(cistella::lock::lock_path());
        let lock_id = lock_meta.as_ref().ok().map(|meta| (meta.dev(), meta.ino()));
        let locks_read = std::fs::read_to_string("/proc/locks");
        // Every route to the deadline names its cause: a read
        // or metadata error is itself the diagnostic (restricted
        // proc, hidepid, missing lock dir) — never a stale or
        // empty summary.
        if let Err(error) = &locks_read {
            last_seen = format!("read /proc/locks failed: {error}");
        } else if lock_id.is_none() {
            last_seen = format!(
                "lock file unstatable: {}",
                cistella::lock::lock_path().display()
            );
        }
        if let (Ok(text), Some((dev, ino))) = (locks_read, lock_id) {
            let waiter = BlockedWait { pid, dev, ino };
            // Relevant lines for the failure message (bounded):
            // any line naming the pid, verbatim, so an
            // unexpected kernel format is reportable, not
            // mysterious. Counts alone would not be.
            let mut relevant: Vec<&str> = Vec::new();
            let matched = text.lines().any(|line| {
                if line.contains(&pid.to_string()) && relevant.len() < 10 {
                    relevant.push(line);
                }
                lock_waiter_matches(line, &waiter)
            });
            if matched {
                return;
            }
            last_seen = format!(
                "{} lock lines; pid-mentioning lines: {:?}",
                text.lines().count(),
                relevant
            );
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                panic!("child {pid} exited ({status:?}) before blocking on the lock")
            }
            Ok(None) => {}
            Err(error) => panic!("child {pid} unobservable: {error}"),
        }
        if Instant::now() > deadline {
            panic!("child {pid} never observably blocked on the lock (last: {last_seen})");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Phase 1, admission: opens the fifo write end O_NONBLOCK.
/// ENXIO means no reader right now (not-yet-admitted): retried
/// boundedly while the workload lives; an exited workload
/// fails fast with the missing-reader diagnostic; the bound
/// fires fail-loud. No O_RDWR anywhere: it would supply our
/// own reader and mask the missing-workload-reader negative.
pub fn admit_fifo(
    gate: &std::path::Path,
    child: &mut std::process::Child,
    timeout: Duration,
) -> std::os::unix::io::OwnedFd {
    use std::os::unix::io::FromRawFd as _;
    let deadline = Instant::now() + timeout;
    loop {
        match nix::fcntl::open(
            gate,
            nix::fcntl::OFlag::O_WRONLY | nix::fcntl::OFlag::O_NONBLOCK,
            nix::sys::stat::Mode::empty(),
        ) {
            // SAFETY: freshly opened above, owned here, wrapped
            // exactly once.
            Ok(raw) => {
                return unsafe { std::os::unix::io::OwnedFd::from_raw_fd(raw) };
            }
            Err(nix::errno::Errno::ENXIO) => {
                // No reader right now: not-yet-admitted (retry
                // while the workload lives) or missing (the
                // workload is dead) — the child status decides,
                // never the clock alone.
                match child.try_wait() {
                    Ok(Some(status)) => panic!(
                        "release impossible: workload exited ({status:?}) before admission (missing reader)"
                    ),
                    Ok(None) => {}
                    Err(error) => panic!("child unobservable: {error}"),
                }
                if Instant::now() > deadline {
                    panic!("release never admitted: no reader within bound");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("release open failed for {}: {error}", gate.display()),
        }
    }
}

/// Phases 2+3, release write: one newline down an admitted
/// fifo. Returns `EPIPE` when the reader vanished after
/// admission (distinct from never-arrived); any other error
/// propagates unchanged.
pub fn write_release(fd: &std::os::unix::io::OwnedFd) -> Result<(), nix::errno::Errno> {
    use std::os::unix::io::AsFd as _;
    match nix::unistd::write(fd.as_fd(), b"\n") {
        Ok(_) => Ok(()),
        Err(error) => Err(error),
    }
}

/// Full release protocol: admission (bounded, missing-reader
/// fail-fast) then one newline. EPIPE on the write means the
/// reader vanished after admission. No worker, no leak — every
/// step is non-blocking by construction.
pub fn release_fifo(gate: &std::path::Path, child: &mut std::process::Child, timeout: Duration) {
    let admitted = admit_fifo(gate, child, timeout);
    match write_release(&admitted) {
        Ok(()) => {}
        Err(nix::errno::Errno::EPIPE) => {
            panic!("release write: reader vanished after admission")
        }
        Err(error) => panic!("release write failed: {error}"),
    }
}

#[cfg(test)]
mod blocking_tests {
    // Fast pins for the observation predicates (no podman, no
    // systemd): the matcher is pure; the fifo phases use local
    // fifos plus ordinary child processes.
    use super::{BlockedWait, admit_fifo, lock_waiter_matches, release_fifo, write_release};

    #[test]
    fn blocked_holder_line_matches_exact_pid_and_file() {
        // Real /proc/locks shape (Tier-1's seat): ordinal,
        // `->` marker, FLOCK, pid, maj:min:ino.
        let waiter = BlockedWait {
            pid: 107891,
            dev: super::makedev_for_test(0x103, 0x02),
            ino: 55212111,
        };
        assert!(
            lock_waiter_matches(
                "80: -> FLOCK  ADVISORY  WRITE 107891 103:02:55212111 0 EOF",
                &waiter
            ),
            "blocked waiter line matches"
        );
    }

    #[test]
    fn holder_without_marker_never_matches() {
        // The negative control that matters: a child that opens
        // the file but never flocks produces a holder line with
        // no `->` marker — the predicate must not satisfy it.
        let waiter = BlockedWait {
            pid: 107889,
            dev: super::makedev_for_test(0x103, 0x02),
            ino: 55212111,
        };
        assert!(
            !lock_waiter_matches(
                "80: FLOCK  ADVISORY  WRITE 107889 103:02:55212111 0 EOF",
                &waiter
            ),
            "unblocked holder must not match"
        );
    }

    #[test]
    fn wrong_pid_or_file_never_matches() {
        let waiter = BlockedWait {
            pid: 107891,
            dev: super::makedev_for_test(0x103, 0x02),
            ino: 55212111,
        };
        assert!(
            !lock_waiter_matches(
                "80: -> FLOCK  ADVISORY  WRITE 999999 103:02:55212111 0 EOF",
                &waiter
            ),
            "another pid's block must not match"
        );
        assert!(
            !lock_waiter_matches(
                "80: -> FLOCK  ADVISORY  WRITE 107891 103:02:99999999 0 EOF",
                &waiter
            ),
            "another file's block must not match"
        );
        assert!(
            !lock_waiter_matches(
                "80: -> POSIX  ADVISORY  WRITE 107891 103:02:55212111 0 EOF",
                &waiter
            ),
            "non-flock locks must not match"
        );
    }

    #[test]
    #[should_panic(expected = "before admission (missing reader)")]
    fn fifo_missing_reader_fails_fast() {
        // Phase-1 negative: the workload exits without ever
        // opening the fifo — release fails fast with the
        // missing-reader diagnostic instead of hanging.
        // Deterministic: `true` is already gone, so the first
        // ENXIO observes the exit. Fast: no bound is consumed.
        let dir = tempfile::tempdir().expect("tmpdir");
        let gate = dir.path().join("gate.fifo");
        nix::unistd::mkfifo(&gate, nix::sys::stat::Mode::from_bits(0o600).expect("mode"))
            .expect("mkfifo");
        let mut child = std::process::Command::new("true")
            .spawn()
            .expect("spawn true");
        let _ = child.wait();
        release_fifo(&gate, &mut child, std::time::Duration::from_secs(10));
    }

    #[test]
    fn fifo_reader_vanished_after_admission_is_distinct() {
        // Phase-2/3 pin: the reader pairs (admission succeeds),
        // then vanishes; the write must surface EPIPE — never
        // the never-arrived diagnostic. Channels order every
        // step; no sleeps, no timing asserts.
        use std::sync::mpsc::channel;
        let dir = tempfile::tempdir().expect("tmpdir");
        let gate = dir.path().join("gate.fifo");
        nix::unistd::mkfifo(&gate, nix::sys::stat::Mode::from_bits(0o600).expect("mode"))
            .expect("mkfifo");
        let (gone_tx, gone_rx) = channel::<()>();
        let gate_path = gate.clone();
        let reader = std::thread::spawn(move || {
            let fd = nix::fcntl::open(
                &gate_path,
                nix::fcntl::OFlag::O_RDONLY,
                nix::sys::stat::Mode::empty(),
            )
            .expect("reader opens");
            // Hold the read end until the test releases us;
            // the test joins this thread before writing, so
            // the vanished-before-write ordering is exact.
            gone_rx.recv().expect("release signal");
            nix::unistd::close(fd).expect("reader closes");
        });
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn keepalive");
        // Admission pairs with the blocked reader...
        let admitted = admit_fifo(&gate, &mut child, std::time::Duration::from_secs(10));
        // ...the reader is released to vanish...
        let _ = gone_tx.send(());
        reader.join().expect("reader exits");
        // ...so the write observes exactly EPIPE.
        let outcome = write_release(&admitted);
        let _ = child.kill();
        assert_eq!(
            outcome,
            Err(nix::errno::Errno::EPIPE),
            "vanished reader surfaces EPIPE, got: {outcome:?}"
        );
    }
}
