//! Operation-log pins: pruning, admission, reservations, and
//! finalization.
//!
//! Split from `operation_log.rs` at the file-size limit; shared
//! fixtures live there.

use super::operation_log::{craft_incomplete_op, state_root};
use cistella::operation_log::RESERVATION_BYTES;
use cistella::operation_log::{Budget, Injected, Operation, Outcome, op_id_valid, read_operation};
#[test]
fn prune_evicts_over_cap_and_skips_live_writer() {
    let root = state_root();
    let log_root = root.path().join("logs");
    let first = Operation::open(&log_root, "gc", None, &Injected::default()).expect("open");
    first.close("success");
    let second = Operation::open(&log_root, "gc", None, &Injected::default()).expect("open");
    second.close("success");
    // Live writer: active, never a victim even over the cap.
    let live = Operation::open(&log_root, "gc", None, &Injected::default()).expect("open");
    let live_dir = live.dir().to_path_buf();
    let budget = Budget {
        total: 64 * 1024 * 1024,
        reserve: 32 * 1024 * 1024,
        success_cap: 1,
        age_secs: 3600,
    };
    let report = cistella::operation_log::prune::prune_with(&log_root, &budget);
    assert_eq!(report.evicted, 1, "one success victim over the cap");
    assert!(live_dir.exists(), "live writer skipped");
    let remaining: Vec<_> = std::fs::read_dir(&log_root)
        .expect("list")
        .flatten()
        .filter(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            entry.file_type().map(|t| t.is_dir()).unwrap_or(false)
                && op_id_valid(&name)
                && name != "evicted"
        })
        .collect();
    assert_eq!(remaining.len(), 2, "one victim plus the live writer");
    let markers: Vec<_> = std::fs::read_dir(log_root.join("evicted"))
        .expect("evicted dir")
        .flatten()
        .collect();
    assert_eq!(markers.len(), 1, "one accounting marker per eviction");
    live.close("success");
}

/// Builds a frame bypassing the encoder: the payload may exceed
/// the maximum, which `encode_frame` would refuse. The JSON stays
/// valid so only the size check can reject the frame.

#[test]
fn live_incomplete_survives_reserve_pressure() {
    // The active-writer guard is load-bearing: this incomplete
    // operation exceeds the failure reserve, so absent the guard
    // it would be the first victim. It survives because its owner
    // is provably alive.
    let root = state_root();
    let log_root = root.path().join("logs");
    let live = Operation::open(&log_root, "gc", None, &Injected::default()).expect("open");
    let live_dir = live.dir().to_path_buf();
    std::mem::forget(live);
    let budget = Budget {
        total: 64 * 1024 * 1024,
        reserve: 64,
        success_cap: 2000,
        age_secs: 3600,
    };
    let report = cistella::operation_log::prune::prune_with(&log_root, &budget);
    assert_eq!(report.evicted, 0, "live writer is never a victim");
    assert!(live_dir.exists(), "live incomplete operation survives");
}

#[test]
fn dead_incomplete_is_evicted_with_accounting() {
    // Inactive-incomplete resolution: an owner that provably cannot
    // match a live process is eligible, and the eviction leaves a
    // marker.
    let root = state_root();
    let log_root = root.path().join("logs");
    let live = Operation::open(&log_root, "gc", None, &Injected::default()).expect("open");
    let live_dir = live.dir().to_path_buf();
    std::mem::forget(live);
    craft_incomplete_op(&log_root, "deadbeef00", std::process::id(), u64::MAX);
    let budget = Budget {
        total: 64 * 1024 * 1024,
        reserve: 64,
        success_cap: 2000,
        age_secs: 3600,
    };
    let report = cistella::operation_log::prune::prune_with(&log_root, &budget);
    assert_eq!(report.evicted, 1, "exactly the dead operation goes");
    assert!(
        !log_root.join("deadbeef00").exists(),
        "dead incomplete evicted"
    );
    assert!(live_dir.exists(), "live incomplete survives");
    assert!(
        report.bytes_reclaimed > 0,
        "eviction accounts reclaimed bytes"
    );
    let markers: Vec<_> = std::fs::read_dir(log_root.join("evicted"))
        .expect("evicted dir")
        .flatten()
        .collect();
    assert_eq!(markers.len(), 1, "one accounting marker per eviction");
}

#[test]
fn finalizing_marker_protects_complete_op() {
    // The finalization window: a complete-looking operation with a
    // marker and a live owner is skipped, while its unmarked twin
    // is a valid victim. Eviction carries an accounting marker.
    let root = state_root();
    let log_root = root.path().join("logs");
    let marked = Operation::open(&log_root, "gc", None, &Injected::default()).expect("open");
    let marked_dir = marked.dir().to_path_buf();
    marked.close("success");
    std::fs::write(marked_dir.join(".finalizing"), b"").expect("marker");
    let plain = Operation::open(&log_root, "gc", None, &Injected::default()).expect("open");
    let plain_dir = plain.dir().to_path_buf();
    plain.close("success");
    let budget = Budget {
        total: 64 * 1024 * 1024,
        reserve: 64 * 1024 * 1024,
        success_cap: 0,
        age_secs: 3600,
    };
    let report = cistella::operation_log::prune::prune_with(&log_root, &budget);
    assert_eq!(report.evicted, 1, "only the unmarked twin goes");
    assert!(marked_dir.exists(), "finalizing operation skipped");
    assert!(!plain_dir.exists(), "completed twin evicted");
    let markers: Vec<_> = std::fs::read_dir(log_root.join("evicted"))
        .expect("evicted dir")
        .flatten()
        .collect();
    assert_eq!(markers.len(), 1, "eviction accounted");
}

#[test]
fn admission_refuses_when_budget_cannot_be_secured() {
    // The admission path is reachable: a budget smaller than one
    // reservation refuses the open, and the business operation
    // proceeds (the error is returned, nothing panics, nothing is
    // created).
    let root = state_root();
    let log_root = root.path().join("logs");
    let denied = Budget {
        total: 100,
        reserve: 0,
        success_cap: 2000,
        age_secs: 3600,
    };
    assert!(
        Operation::open_with(&log_root, "gc", None, &Injected::default(), &denied).is_err(),
        "budget refuses the open"
    );
    let ops: Vec<_> = std::fs::read_dir(&log_root)
        .map(|listing| {
            listing
                .flatten()
                .filter(|e| cistella::operation_log::op_id_valid(&e.file_name().to_string_lossy()))
                .collect()
        })
        .unwrap_or_default();
    assert!(ops.is_empty(), "refused open creates no operation");
}

#[test]
fn reservation_releases_on_close() {
    // Closing frees the held reservation: a budget fitting exactly
    // one open admits a second operation only after the first
    // closes.
    let root = state_root();
    let log_root = root.path().join("logs");
    let budget = Budget {
        total: 2 * RESERVATION_BYTES - 1,
        reserve: 0,
        success_cap: 2000,
        age_secs: 3600,
    };
    let first = Operation::open_with(&log_root, "gc", None, &Injected::default(), &budget)
        .expect("first open fits");
    assert!(
        Operation::open_with(&log_root, "gc", None, &Injected::default(), &budget).is_err(),
        "second open refused while reserved"
    );
    first.close("success");
    Operation::open_with(&log_root, "gc", None, &Injected::default(), &budget)
        .expect("released reservation admits the next open");
}

#[test]
fn live_reservation_blocks_second_open() {
    // Sequential and deterministic: one leaked open holds its
    // reservation under an exact two-shares-minus-one budget, so a
    // second open is refused BECAUSE the live reservation is
    // accounted, not because of lock contention. Dropping the
    // reservation from the admission check would admit it.
    let root = state_root();
    let log_root = root.path().join("logs");
    // Exact boundary: two shares fit, three do not, and usage is
    // always non-negative - so the first open fits and the second
    // is refused if and only if the live reservation is counted.
    let budget = Budget {
        total: 2 * RESERVATION_BYTES - 1,
        reserve: 0,
        success_cap: 2000,
        age_secs: 3600,
    };
    let held = Operation::open_with(&log_root, "gc", None, &Injected::default(), &budget)
        .expect("first open fits");
    std::mem::forget(held);
    assert!(
        Operation::open_with(&log_root, "gc", None, &Injected::default(), &budget).is_err(),
        "live reservation is accounted"
    );
}

#[test]
fn stale_reservations_are_reaped() {
    // A reservation whose owner is provably dead does not block
    // admission: the next open reaps it and proceeds.
    let root = state_root();
    let log_root = root.path().join("logs");
    std::fs::create_dir_all(log_root.join(".reserved")).expect("reserved dir");
    let stale = serde_json::json!({
        "op": "stalebeef00",
        "pid": std::process::id(),
        "process_start": u64::MAX,
        "bytes": RESERVATION_BYTES,
    })
    .to_string();
    let stale_path = log_root.join(".reserved").join("stalebeef00.json");
    std::fs::write(&stale_path, stale).expect("stale reservation");
    // Exact budget: the stale file's own bytes plus one metadata
    // share plus one reservation fit, but two reservations do not.
    // Admission succeeds only if the dead entry is reaped first.
    let stale_bytes = std::fs::metadata(&stale_path).expect("stat").len();
    let budget = Budget {
        total: 4096 + stale_bytes + RESERVATION_BYTES,
        reserve: 0,
        success_cap: 2000,
        age_secs: 3600,
    };
    Operation::open_with(&log_root, "gc", None, &Injected::default(), &budget)
        .expect("stale reservation reaped, open proceeds");
    assert!(!stale_path.exists(), "dead reservation reaped");
}

#[test]
fn concurrent_opens_respect_the_budget() {
    // Concurrent opens serialize on the admission lock; the number
    // of grants never exceeds what the budget accounts for. Each
    // grant is leaked (forgotten) so its reservation stays live
    // for the count below.
    let root = state_root();
    let log_root = root.path().join("logs");
    let budget = Budget {
        total: 3 * RESERVATION_BYTES + 65536,
        reserve: 0,
        success_cap: 2000,
        age_secs: 3600,
    };
    let grants = std::sync::Mutex::new(0u32);
    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| {
                if let Ok(op) =
                    Operation::open_with(&log_root, "gc", None, &Injected::default(), &budget)
                {
                    std::mem::forget(op);
                    *grants.lock().expect("mutex") += 1;
                }
            });
        }
    });
    let holders = *grants.lock().expect("mutex");
    assert!(holders >= 1, "at least one open is granted");
    assert!(
        u64::from(holders) * RESERVATION_BYTES <= budget.total,
        "grants fit the budget, got: {holders}"
    );
}

#[test]
fn select_then_evict_sees_a_planted_marker() {
    // The deterministic interleaving pin: selection sees no marker
    // and selects the victim; planting one before eviction must
    // make the unlink-time recheck skip it.
    let root = state_root();
    let log_root = root.path().join("logs");
    let op = Operation::open(&log_root, "gc", None, &Injected::default()).expect("open");
    let dir = op.dir().to_path_buf();
    op.close("success");
    let budget = Budget {
        total: 64 * 1024 * 1024,
        reserve: 64 * 1024 * 1024,
        success_cap: 0,
        age_secs: 3600,
    };
    let selection = cistella::operation_log::prune::select_victims(&log_root, &budget);
    assert_eq!(selection.victims.len(), 1, "selected without marker");
    std::fs::write(dir.join(".finalizing"), b"").expect("plant marker");
    let report = cistella::operation_log::prune::evict(&log_root, &selection.victims, &budget);
    assert_eq!(report.evicted, 0, "recheck skips the marked operation");
    assert_eq!(report.skipped, 1, "skip is counted, not silent");
    assert!(dir.exists(), "marked operation survives");
    std::fs::remove_file(dir.join(".finalizing")).expect("unplant");
    let report = cistella::operation_log::prune::evict(&log_root, &selection.victims, &budget);
    assert_eq!(report.evicted, 1, "unmarked victim is evicted");
}

#[test]
fn contention_skips_logging_without_failing() {
    // Holding the admission lock externally forces the try-lock to
    // fail: the open is refused and the business operation is
    // unaffected (no error is raised to the caller beyond the log
    // skip, and nothing is created).
    use std::os::unix::fs::DirBuilderExt;
    let root = state_root();
    let log_root = root.path().join("logs");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&log_root)
        .expect("mkdir");
    let lock_path = log_root.join(".admission.lock");
    std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .expect("lock file");
    // A second owned fd, so the test holds the flock without
    // moving the original handle anywhere.
    let held = std::fs::OpenOptions::new()
        .write(true)
        .open(&lock_path)
        .expect("lock file again");
    let _guard = nix::fcntl::Flock::lock(held, nix::fcntl::FlockArg::LockExclusive)
        .expect("test holds the lock");
    let budget = Budget {
        total: 64 * 1024 * 1024,
        reserve: 0,
        success_cap: 2000,
        age_secs: 3600,
    };
    assert!(
        Operation::open_with(&log_root, "gc", None, &Injected::default(), &budget).is_err(),
        "contended admission skips logging"
    );
}

#[test]
fn dead_owner_is_known_absent() {
    // A reaped child proves the absent-/proc-entry path: the owner
    // is certainly dead, not merely unqueryable.
    let mut child = std::process::Command::new("/bin/true")
        .spawn()
        .expect("spawn");
    let pid = child.id();
    child.wait().expect("reap");
    let owner = cistella::operation_log::Owner {
        pid,
        starttime: u64::MAX,
    };
    assert_eq!(
        cistella::operation_log::owner_alive(&owner),
        Some(false),
        "reaped pid reads dead"
    );
}

#[test]
fn abandoned_empty_directory_ages_out() {
    // An operation directory that never received records carries no
    // evidence; once past the age bound it is an ordinary victim.
    let root = state_root();
    let log_root = root.path().join("logs");
    std::fs::create_dir_all(&log_root).expect("mkdir");
    let dir = log_root.join("abc123empty");
    std::fs::create_dir(&dir).expect("mkdir");
    let past = std::time::SystemTime::now() - std::time::Duration::from_secs(8 * 24 * 3600);
    std::fs::File::options()
        .read(true)
        .open(&dir)
        .and_then(|f| f.set_modified(past))
        .expect("backdate");
    let budget = Budget {
        total: 64 * 1024 * 1024,
        reserve: 64 * 1024 * 1024,
        success_cap: 2000,
        age_secs: 7 * 24 * 3600,
    };
    let report = cistella::operation_log::prune::prune_with(&log_root, &budget);
    assert_eq!(report.evicted, 1, "aged empty directory reaped");
    assert!(!dir.exists(), "no immortal empties");
}

#[test]
fn pre_construction_failure_releases_the_reservation() {
    // A failure after the reservation is written (here: the op
    // directory path already exists as a file) must free the
    // reservation instead of leaking it: a second open with an
    // exact-fit budget then succeeds.
    let root = state_root();
    let log_root = root.path().join("logs");
    std::fs::create_dir_all(&log_root).expect("mkdir");
    let budget = Budget {
        total: 2 * RESERVATION_BYTES - 1,
        reserve: 0,
        success_cap: 2000,
        age_secs: 3600,
    };
    std::fs::write(log_root.join("blockedbeef"), b"blocker").expect("blocker");
    assert!(
        Operation::open_with_id(
            &log_root,
            "gc",
            None,
            &Injected::default(),
            &budget,
            "blockedbeef"
        )
        .is_err(),
        "directory creation fails on the blocker file"
    );
    assert!(
        !log_root.join(".reserved").join("blockedbeef.json").exists(),
        "failed open holds no reservation"
    );
    assert!(
        Operation::open_with(&log_root, "gc", None, &Injected::default(), &budget).is_ok(),
        "budget intact after the failed open"
    );
}

#[test]
fn planted_eviction_marker_does_not_escape() {
    // A symlink planted where an eviction marker will be written
    // must not truncate its target: open fails closed, the outside
    // file is byte-identical, and the victim is still reaped.
    let root = state_root();
    let log_root = root.path().join("logs");
    let op = Operation::open(&log_root, "gc", None, &Injected::default()).expect("open");
    let dir = op.dir().to_path_buf();
    let name = dir.file_name().expect("name").to_string_lossy().to_string();
    op.close("success");
    let outside = root.path().join("outside.txt");
    std::fs::write(&outside, b"untouched").expect("sentinel");
    std::fs::create_dir_all(log_root.join("evicted")).expect("evicted dir");
    std::os::unix::fs::symlink(
        &outside,
        log_root.join("evicted").join(format!("{name}.json")),
    )
    .expect("plant symlink");
    let budget = Budget {
        total: 64 * 1024 * 1024,
        reserve: 64 * 1024 * 1024,
        success_cap: 0,
        age_secs: 3600,
    };
    let report = cistella::operation_log::prune::prune_with(&log_root, &budget);
    assert_eq!(report.evicted, 1, "victim still reaped");
    assert_eq!(
        std::fs::read(&outside).expect("read sentinel"),
        b"untouched",
        "outside file untouched"
    );
}

#[test]
fn planted_admission_lock_is_refused() {
    // A symlink (or FIFO) at the admission-lock path must fail the
    // open closed instead of following it - the business operation
    // proceeds without logging.
    let root = state_root();
    let log_root = root.path().join("logs");
    std::fs::create_dir_all(&log_root).expect("mkdir");
    std::fs::write(log_root.join("real-lock-target"), b"x").expect("write");
    std::os::unix::fs::symlink(
        log_root.join("real-lock-target"),
        log_root.join(".admission.lock"),
    )
    .expect("plant symlink");
    let budget = Budget {
        total: 64 * 1024 * 1024,
        reserve: 0,
        success_cap: 2000,
        age_secs: 3600,
    };
    assert!(
        Operation::open_with(&log_root, "gc", None, &Injected::default(), &budget).is_err(),
        "symlinked lock refuses the open"
    );
}

#[test]
fn contended_lock_defers_victims_without_failing() {
    // Holding the admission lock externally forces prune's
    // unlink-time recheck to defer: the victim is skipped and
    // counted, never force-unlinked, never counted as failed.
    let root = state_root();
    let log_root = root.path().join("logs");
    let op = Operation::open(&log_root, "gc", None, &Injected::default()).expect("open");
    let dir = op.dir().to_path_buf();
    op.close("success");
    std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(log_root.join(".admission.lock"))
        .expect("lock file");
    let held = std::fs::OpenOptions::new()
        .write(true)
        .open(log_root.join(".admission.lock"))
        .expect("second handle");
    let _guard = nix::fcntl::Flock::lock(held, nix::fcntl::FlockArg::LockExclusive)
        .expect("test holds the admission lock");
    let budget = Budget {
        total: 64 * 1024 * 1024,
        reserve: 0,
        success_cap: 0,
        age_secs: 3600,
    };
    let selection = cistella::operation_log::prune::select_victims(&log_root, &budget);
    assert_eq!(selection.victims.len(), 1, "selected before the lock");
    let report = cistella::operation_log::prune::evict(&log_root, &selection.victims, &budget);
    assert_eq!(report.evicted, 0, "contended lock defers eviction");
    assert_eq!(report.skipped, 1, "deferral is counted, not failed");
    assert_eq!(report.eviction_failures, 0, "no failure recorded");
    assert!(dir.exists(), "deferred victim survives");
    drop(_guard);
    let report = cistella::operation_log::prune::evict(&log_root, &selection.victims, &budget);
    assert_eq!(report.evicted, 1, "released lock lets eviction through");
}

#[test]
fn open_with_id_rejects_unvalidated_ids() {
    // Explicit ids must satisfy the path-safe alphabet before any
    // reservation is written: no reservation, no directory.
    let root = state_root();
    let log_root = root.path().join("logs");
    std::fs::create_dir_all(&log_root).expect("mkdir");
    let budget = Budget {
        total: 64 * 1024,
        reserve: 0,
        success_cap: 2000,
        age_secs: 3600,
    };
    assert!(
        Operation::open_with_id(
            &log_root,
            "gc",
            None,
            &Injected::default(),
            &budget,
            "../evil"
        )
        .is_err(),
        "non-alphabet id refused"
    );
    let reserved: Vec<_> = std::fs::read_dir(log_root.join(".reserved"))
        .map(|l| l.collect())
        .unwrap_or_else(|_| Vec::new());
    assert!(reserved.is_empty(), "refused open leaves no reservation");
}

#[test]
fn reaped_child_reads_absent_not_unknown() {
    // An actually reaped child has no /proc entry at all: the
    // owner reads as certainly dead, not merely unqueryable.
    // (Mismatched-starttime fixtures prove the comparison path;
    // this proves the absence path.)
    let mut child = std::process::Command::new("/bin/true")
        .spawn()
        .expect("spawn");
    let pid = child.id();
    assert!(child.wait().expect("reap").success());
    // Reap first so no zombie lingers: the pid must be gone.
    let owner = cistella::operation_log::Owner { pid, starttime: 0 };
    assert_eq!(
        cistella::operation_log::owner_alive(&owner),
        Some(false),
        "reaped pid reads dead, not unknown"
    );
}

#[test]
fn planted_fifo_admission_lock_is_refused() {
    // A FIFO at the admission-lock path must fail the open
    // closed: no blocking open, no business impact, nothing
    // created.
    let root = state_root();
    let log_root = root.path().join("logs");
    std::fs::create_dir_all(&log_root).expect("mkdir");
    nix::unistd::mkfifo(
        log_root.join(".admission.lock").as_path(),
        nix::sys::stat::Mode::from_bits_truncate(0o600),
    )
    .expect("plant fifo");
    let budget = Budget {
        total: 64 * 1024 * 1024,
        reserve: 0,
        success_cap: 2000,
        age_secs: 3600,
    };
    assert!(
        Operation::open_with(&log_root, "gc", None, &Injected::default(), &budget).is_err(),
        "FIFO lock refuses the open"
    );
    assert!(
        std::fs::read_dir(&log_root)
            .expect("list")
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name().to_string_lossy().starts_with("cistella-")
                    || e.file_name()
                        .to_string_lossy()
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
            })
            .count()
            == 0,
        "no operation directory created"
    );
}

#[test]
fn close_triggers_maintenance_on_aged_victims() {
    // A normal close runs its own maintenance pass: an aged
    // completed operation is evicted with an accounting marker.
    // This pins the U1 fix — holding the admission lock across
    // prune would make every close skip its own evictions.
    let root = state_root();
    let log_root = root.path().join("logs");
    let old = Operation::open(&log_root, "gc", None, &Injected::default()).expect("open");
    let old_dir = old.dir().to_path_buf();
    old.close("success");
    let past = std::time::SystemTime::now() - std::time::Duration::from_secs(8 * 24 * 3600);
    std::fs::File::open(&old_dir)
        .and_then(|f| f.set_modified(past))
        .expect("backdate");
    let fresh = Operation::open(&log_root, "gc", None, &Injected::default()).expect("open");
    let fresh_dir = fresh.dir().to_path_buf();
    fresh.close("success");
    assert!(
        !old_dir.exists(),
        "close-triggered maintenance evicts the aged victim"
    );
    assert!(fresh_dir.exists(), "fresh operation survives its own close");
    let markers: Vec<_> = std::fs::read_dir(log_root.join("evicted"))
        .expect("evicted dir")
        .flatten()
        .collect();
    assert_eq!(markers.len(), 1, "eviction leaves its accounting marker");
}

#[test]
fn contended_close_leaves_incomplete_no_terminal() {
    // When the admission lock cannot be secured, close skips the
    // terminal write instead of finalizing unprotected: the log
    // stays explicitly incomplete and the business operation is
    // unaffected.
    let root = state_root();
    let log_root = root.path().join("logs");
    std::fs::create_dir_all(&log_root).expect("mkdir");
    let held = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(log_root.join(".admission.lock"))
        .expect("lock file");
    let _guard = nix::fcntl::Flock::lock(held, nix::fcntl::FlockArg::LockExclusive)
        .expect("test holds the lock");
    let budget = Budget {
        total: 64 * 1024 * 1024,
        reserve: 0,
        success_cap: 2000,
        age_secs: 3600,
    };
    // Open must fail while the lock is held (admission refuses).
    assert!(Operation::open_with(&log_root, "gc", None, &Injected::default(), &budget).is_err());
    drop(_guard);
    // With the lock free, open succeeds; re-hold it across close.
    let op = Operation::open(&log_root, "gc", None, &Injected::default()).expect("open");
    let dir = op.dir().to_path_buf();
    let held = std::fs::OpenOptions::new()
        .write(true)
        .open(log_root.join(".admission.lock"))
        .expect("lock file");
    let _guard = nix::fcntl::Flock::lock(held, nix::fcntl::FlockArg::LockExclusive)
        .expect("test holds the lock");
    op.close("success");
    drop(_guard);
    let read = read_operation(&dir).expect("read");
    assert!(
        matches!(read.outcome, Outcome::Unknown),
        "contended close leaves no terminal"
    );
    assert!(
        !read
            .records
            .iter()
            .any(|r| r.get("kind").and_then(|v| v.as_str()) == Some("terminal")),
        "no terminal was written unprotected"
    );
    assert!(
        dir.exists(),
        "unfinalized operation is not reaped by its own close"
    );
}

#[test]
fn fifo_reservation_cannot_block_admission() {
    // A planted FIFO in the reservation dir must neither block
    // the open nor count as capacity: our writer only creates
    // regular files. Before the bounded reader this hung on a
    // blocking open while holding the admission lock.
    let root = state_root();
    let log_root = root.path().join("logs");
    std::fs::create_dir_all(log_root.join(".reserved")).expect("reserved dir");
    nix::unistd::mkfifo(
        &log_root.join(".reserved").join("planted.json"),
        nix::sys::stat::Mode::from_bits(0o600).expect("mode"),
    )
    .expect("mkfifo");
    let budget = Budget {
        total: 64 * 1024 * 1024,
        reserve: 0,
        success_cap: 2000,
        age_secs: 3600,
    };
    Operation::open_with(&log_root, "gc", None, &Injected::default(), &budget)
        .expect("open skips the non-regular reservation");
}

#[test]
fn invalid_reservation_is_capacity_not_zero() {
    // Entries the scan cannot account for — invalid JSON, or
    // well-formed JSON with no byte count — charge one full
    // reservation share instead of vanishing. Exact boundary: the
    // plant's own file bytes plus the directory share plus two
    // shares fit, but three do not, so the open is refused if
    // and only if the plant is charged.
    for (name, content) in [
        ("bad.json", "not json at all".to_string()),
        ("nobytes.json", serde_json::json!({"op": "x"}).to_string()),
    ] {
        let root = state_root();
        let log_root = root.path().join("logs");
        std::fs::create_dir_all(log_root.join(".reserved")).expect("reserved dir");
        std::fs::write(log_root.join(".reserved").join(name), &content).expect("plant");
        let budget = Budget {
            total: 4096 + content.len() as u64 + 2 * RESERVATION_BYTES - 1,
            reserve: 0,
            success_cap: 2000,
            age_secs: 3600,
        };
        assert!(
            Operation::open_with(&log_root, "gc", None, &Injected::default(), &budget).is_err(),
            "unaccountable reservation is charged, not zeroed: {name}"
        );
    }
}

#[test]
fn overfull_reservation_listing_refuses_logging() {
    // Bounded enumeration: past the entry bound the open is
    // refused (logging skipped, business unaffected) rather than
    // scanning an unbounded planted listing under the lock.
    let root = state_root();
    let log_root = root.path().join("logs");
    std::fs::create_dir_all(log_root.join(".reserved")).expect("reserved dir");
    for i in 0..4200u32 {
        std::fs::write(
            log_root.join(".reserved").join(format!("p{i:05}.json")),
            b"{}",
        )
        .expect("plant");
    }
    let budget = Budget {
        total: u64::MAX,
        reserve: 0,
        success_cap: 2000,
        age_secs: 3600,
    };
    assert!(
        Operation::open_with(&log_root, "gc", None, &Injected::default(), &budget).is_err(),
        "unbounded listing refuses rather than admitting blind"
    );
}

#[test]
fn replaced_records_fifo_does_not_block_prune() {
    // A records file replaced with a FIFO after close must not
    // stall the prune scan: the candidate reads UNKNOWN and the
    // normal age rules dispose of it.
    let root = state_root();
    let log_root = root.path().join("logs");
    let op = Operation::open(&log_root, "gc", None, &Injected::default()).expect("open");
    let dir = op.dir().to_path_buf();
    op.close("success");
    std::fs::remove_file(dir.join("records.log")).expect("remove records");
    nix::unistd::mkfifo(
        &dir.join("records.log"),
        nix::sys::stat::Mode::from_bits(0o600).expect("mode"),
    )
    .expect("mkfifo");
    let past = std::time::SystemTime::now() - std::time::Duration::from_secs(8 * 24 * 3600);
    std::fs::File::open(&dir)
        .and_then(|f| f.set_modified(past))
        .expect("backdate");
    let budget = Budget {
        total: 64 * 1024 * 1024,
        reserve: 64 * 1024 * 1024,
        success_cap: 0,
        age_secs: 3600,
    };
    let report = cistella::operation_log::prune::prune_with(&log_root, &budget);
    assert_eq!(report.evicted, 1, "fifo-replaced candidate reaped");
    assert!(!dir.exists(), "reap completes without blocking");
}

#[test]
fn sub_share_byte_claims_are_charged_a_full_share() {
    // Production reservations are one fixed quantum: the byte
    // claim is never trusted. Zero, sub-share, and unbounded
    // claims all charge one full share, never their face value.
    // Same exact boundary as the invalid-JSON pin, tabulated over
    // the concrete amounts.
    for claimed in [0u64, 1, RESERVATION_BYTES - 1, u64::MAX] {
        let root = state_root();
        let log_root = root.path().join("logs");
        std::fs::create_dir_all(log_root.join(".reserved")).expect("reserved dir");
        let content = serde_json::json!({"op": "s", "bytes": claimed}).to_string();
        std::fs::write(log_root.join(".reserved").join("s.json"), &content).expect("plant");
        let budget = Budget {
            total: 4096 + content.len() as u64 + 2 * RESERVATION_BYTES - 1,
            reserve: 0,
            success_cap: 2000,
            age_secs: 3600,
        };
        assert!(
            Operation::open_with(&log_root, "gc", None, &Injected::default(), &budget).is_err(),
            "byte claim {claimed} is charged a full share, not trusted"
        );
    }
}

#[test]
fn missing_start_identity_cannot_prove_death() {
    // A reservation naming a live pid with no process_start must
    // read as unknown, never as a fabricated mismatching identity:
    // it survives (no reaping) and charges a full share.
    let root = state_root();
    let log_root = root.path().join("logs");
    std::fs::create_dir_all(log_root.join(".reserved")).expect("reserved dir");
    let content = serde_json::json!({
        "op": "m",
        "pid": std::process::id(),
        "bytes": RESERVATION_BYTES,
    })
    .to_string();
    let plant = log_root.join(".reserved").join("m.json");
    std::fs::write(&plant, &content).expect("plant");
    let budget = Budget {
        total: 4096 + content.len() as u64 + 2 * RESERVATION_BYTES - 1,
        reserve: 0,
        success_cap: 2000,
        age_secs: 3600,
    };
    assert!(
        Operation::open_with(&log_root, "gc", None, &Injected::default(), &budget).is_err(),
        "identity-unknown reservation is charged, not reaped"
    );
    assert!(
        plant.exists(),
        "missing start identity never proves death: no reaping"
    );
}
