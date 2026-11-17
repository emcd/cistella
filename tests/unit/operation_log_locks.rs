//! Operation-log pins: admission locks, close contention, and fifo
//! ordering.
//!
//! Split from `operation_log_prune.rs` at the file-size limit; shared
//! fixtures live in `operation_log.rs`.

use super::operation_log::state_root;
use cistella::operation_log::{
    Budget, Injected, Operation, Outcome, read_operation,
};
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
        Operation::open_with(
            &log_root,
            "gc",
            None,
            &Injected::default(),
            &budget
        )
        .is_err(),
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
    let op = Operation::open(&log_root, "gc", None, &Injected::default())
        .expect("open");
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
    let _guard =
        nix::fcntl::Flock::lock(held, nix::fcntl::FlockArg::LockExclusive)
            .expect("test holds the admission lock");
    let budget = Budget {
        total: 64 * 1024 * 1024,
        reserve: 0,
        success_cap: 0,
        age_secs: 3600,
    };
    let selection =
        cistella::operation_log::prune::select_victims(&log_root, &budget);
    assert_eq!(selection.victims.len(), 1, "selected before the lock");
    let report = cistella::operation_log::prune::evict(
        &log_root,
        &selection.victims,
        &budget,
    );
    assert_eq!(report.evicted, 0, "contended lock defers eviction");
    assert_eq!(report.skipped, 1, "deferral is counted, not failed");
    assert_eq!(report.eviction_failures, 0, "no failure recorded");
    assert!(dir.exists(), "deferred victim survives");
    drop(_guard);
    let report = cistella::operation_log::prune::evict(
        &log_root,
        &selection.victims,
        &budget,
    );
    assert_eq!(report.evicted, 1, "released lock lets eviction through");
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
        Operation::open_with(
            &log_root,
            "gc",
            None,
            &Injected::default(),
            &budget
        )
        .is_err(),
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
    let old = Operation::open(&log_root, "gc", None, &Injected::default())
        .expect("open");
    let old_dir = old.dir().to_path_buf();
    old.close("success");
    let past = std::time::SystemTime::now()
        - std::time::Duration::from_secs(8 * 24 * 3600);
    std::fs::File::open(&old_dir)
        .and_then(|f| f.set_modified(past))
        .expect("backdate");
    let fresh = Operation::open(&log_root, "gc", None, &Injected::default())
        .expect("open");
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
    let _guard =
        nix::fcntl::Flock::lock(held, nix::fcntl::FlockArg::LockExclusive)
            .expect("test holds the lock");
    let budget = Budget {
        total: 64 * 1024 * 1024,
        reserve: 0,
        success_cap: 2000,
        age_secs: 3600,
    };
    // Open must fail while the lock is held (admission refuses).
    assert!(
        Operation::open_with(
            &log_root,
            "gc",
            None,
            &Injected::default(),
            &budget
        )
        .is_err()
    );
    drop(_guard);
    // With the lock free, open succeeds; re-hold it across close.
    let op = Operation::open(&log_root, "gc", None, &Injected::default())
        .expect("open");
    let dir = op.dir().to_path_buf();
    let held = std::fs::OpenOptions::new()
        .write(true)
        .open(log_root.join(".admission.lock"))
        .expect("lock file");
    let _guard =
        nix::fcntl::Flock::lock(held, nix::fcntl::FlockArg::LockExclusive)
            .expect("test holds the lock");
    op.close("success");
    drop(_guard);
    let read = read_operation(&dir).expect("read");
    assert!(
        matches!(read.outcome, Outcome::Unknown),
        "contended close leaves no terminal"
    );
    assert!(
        !read.records.iter().any(
            |r| r.get("kind").and_then(|v| v.as_str()) == Some("terminal")
        ),
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
fn replaced_records_fifo_does_not_block_prune() {
    // A records file replaced with a FIFO after close must not
    // stall the prune scan: the candidate reads UNKNOWN and the
    // normal age rules dispose of it.
    let root = state_root();
    let log_root = root.path().join("logs");
    let op = Operation::open(&log_root, "gc", None, &Injected::default())
        .expect("open");
    let dir = op.dir().to_path_buf();
    op.close("success");
    std::fs::remove_file(dir.join("records.log")).expect("remove records");
    nix::unistd::mkfifo(
        &dir.join("records.log"),
        nix::sys::stat::Mode::from_bits(0o600).expect("mode"),
    )
    .expect("mkfifo");
    let past = std::time::SystemTime::now()
        - std::time::Duration::from_secs(8 * 24 * 3600);
    std::fs::File::open(&dir)
        .and_then(|f| f.set_modified(past))
        .expect("backdate");
    let budget = Budget {
        total: 64 * 1024 * 1024,
        reserve: 64 * 1024 * 1024,
        success_cap: 0,
        age_secs: 3600,
    };
    let report =
        cistella::operation_log::prune::prune_with(&log_root, &budget);
    assert_eq!(report.evicted, 1, "fifo-replaced candidate reaped");
    assert!(!dir.exists(), "reap completes without blocking");
}
