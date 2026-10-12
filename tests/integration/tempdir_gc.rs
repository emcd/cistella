//! Tempdir service live proofs: gc lease discipline, teardown
//! reap, and hooked `/tmp` admission through production conduct.
//!
//! The gc tests drive the real `gc` binary against hand-built
//! leases, slices, and guest processes (no conduct orchestration
//! needed for the lease logic); the conduct tests run full
//! sessions with the reference tempdir fixture through the seat
//! registry.

use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::time::{Duration, Instant};

use super::helpers::*;

fn tempdir_peer_path() -> std::path::PathBuf {
    use std::os::unix::fs::MetadataExt;
    let my_path = std::env::current_exe().expect("current_exe");
    let examples_dir = my_path
        .ancestors()
        .nth(2)
        .expect("target/<profile>/deps ancestors")
        .join("examples");
    let mut candidates: Vec<(std::time::SystemTime, std::path::PathBuf)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&examples_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name != "tempdir_guest" && !name.starts_with("tempdir_guest-") {
                continue;
            }
            let metadata = entry.metadata().expect("metadata");
            if !metadata.is_file() || (metadata.mode() & 0o111) == 0 {
                continue;
            }
            candidates.push((metadata.modified().expect("mtime"), entry.path()));
        }
    }
    candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.0));
    let selected = candidates
        .into_iter()
        .next()
        .map(|(_, path)| path)
        .unwrap_or_else(|| {
            panic!(
                "example binary tempdir_guest not found in {}; run `cargo build --examples` first",
                examples_dir.display()
            )
        });
    // Registry verification refuses group/world-writable guest
    // binaries, and cargo honors the host umask (0002 builders
    // emit 775): normalize the peer to 755 at resolution so
    // every consumer conducts a conforming binary.
    std::fs::set_permissions(&selected, std::fs::Permissions::from_mode(0o755))
        .expect("chmod peer binary");
    selected
}

fn peer_digest(path: &std::path::Path) -> String {
    use sha2::Digest;
    let bytes = std::fs::read(path).expect("read peer binary");
    hex::encode(sha2::Sha256::digest(&bytes))
}

/// Writes a seat registry with tempdir entries into an isolated
/// XDG config dir. Conducts take it via `XDG_CONFIG_HOME`;
/// `gc`/`terminate` never read the registry.
fn write_tempdir_registry() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let digest = peer_digest(&tempdir_peer_path());
    let text = format!(
        "format-version = 1\n\
         [[extensions]]\n\
         name = \"tempdir-test\"\n\
         role = \"tempdir\"\n\
         protocol = 1\n\
         source-digest = {{ algorithm = 'sha256', value = \"{digest}\" }}\n\
         source-path = \"{}\"\n",
        tempdir_peer_path().display()
    );
    let config = dir.path().join("cistella");
    std::fs::create_dir_all(&config).expect("mkdir config");
    std::fs::write(config.join("registry.toml"), text).expect("write registry");
    std::fs::set_permissions(
        config.join("registry.toml"),
        std::fs::Permissions::from_mode(0o644),
    )
    .expect("chmod");
    dir
}

/// Spawns a hanging fake_guest (hello, then sleeps) in its own
/// process group: stands in for a paused pre-response prepare
/// guest. The caller owns quiescence (kill, then wait).
fn spawn_hanging_guest(examples: &std::path::Path) -> std::process::Child {
    use std::os::unix::process::CommandExt;
    let mut child = Command::new(examples.join("fake_guest"));
    // SAFETY: setpgid in the child before exec is the standard
    // process-group setup.
    unsafe {
        child.pre_exec(|| {
            if nix::libc::setpgid(0, 0) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
    child
        .arg("--mode=hang-request")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn hanging guest")
}

fn signal_dead(pid: u32) -> bool {
    matches!(
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None),
        Err(nix::errno::Errno::ESRCH)
    )
}

fn group_dead(pgid: u32) -> bool {
    matches!(
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(-(pgid as i32)), None),
        Err(nix::errno::Errno::ESRCH)
    )
}

fn kill_wait(pid: u32) {
    let _ = nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(pid as i32),
        nix::sys::signal::Signal::SIGKILL,
    );
}

fn wait_group_dead(pgid: u32) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !group_dead(pgid) {
        if Instant::now() >= deadline {
            panic!("timed out waiting group {pgid} to die");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn seat_root() -> std::path::PathBuf {
    cistella::cleanup::seat_temp_root()
}

fn lease_path(session: &str) -> std::path::PathBuf {
    cistella::lock::PrepareLease::path_for(&cistella::lock::runtime_base(), session)
        .expect("lease path forms")
}

fn write_lease(session: &str, content: &str) {
    let path = lease_path(session);
    std::fs::create_dir_all(path.parent().expect("lease has a parent")).expect("mkdir lease dir");
    std::fs::write(&path, content).expect("write lease");
    // Normalize: a permissive build-host umask (e.g. 0002 fixture
    // fallout) must not leave group-writable lease files behind.
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod lease");
}

fn slice_path(session: &str, rand: &str) -> std::path::PathBuf {
    seat_root().join(format!("cistella-tmp-{session}-{rand}"))
}

fn make_slice(session: &str, rand: &str) -> std::path::PathBuf {
    let path = slice_path(session, rand);
    std::fs::create_dir(&path).expect("mkdir slice");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    path
}

#[ignore = "live: requires podman (gc invokes podman ps)"]
#[test]
fn gc_preserves_uncertainty_then_reaps_after_quiescence() {
    let session = format!(
        "livetest-gc-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
            % 1_000_000
    );
    let home = home_dir();
    // Independent session B stays live throughout (its own
    // driver-hold): proves per-session independence of gc
    // decisions.
    let session_b = format!("{session}-b");
    let driver_b = Command::new("/bin/sleep")
        .arg("300")
        .spawn()
        .expect("spawn driver stand-in")
        .id();
    let slice_b = make_slice(&session_b, "999-888");
    write_lease(&session_b, &format!("driver {driver_b}\n"));
    // Session A: driver stand-in plus hanging guest, both recorded.
    let mut driver_a = Command::new("/bin/sleep")
        .arg("300")
        .spawn()
        .expect("spawn driver stand-in");
    let driver_a_pid = driver_a.id();
    let examples = tempdir_peer_path()
        .parent()
        .expect("examples dir")
        .to_path_buf();
    let mut hanging = spawn_hanging_guest(&examples);
    let guest_pid = hanging.id();
    let guest_pgid = guest_pid;
    let slice_a = make_slice(&session, "111-222");
    write_lease(
        &session,
        &format!("driver {driver_a_pid}\nguest {guest_pid} {guest_pgid}\n"),
    );
    // Live driver: gc skips everything.
    let out = run_cistella(&home, &["gc"]);
    assert!(
        out.status.success(),
        "gc: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(slice_a.exists(), "live driver: slice survives");
    assert!(slice_b.exists(), "live driver: independent slice survives");
    // SIGKILL the driver while the guest hangs: uncertainty is
    // preserved (guest group alive) — no reap.
    kill_wait(driver_a_pid);
    let _ = driver_a.wait();
    assert!(signal_dead(driver_a_pid), "driver reaped");
    let out = run_cistella(&home, &["gc"]);
    assert!(
        out.status.success(),
        "gc: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        slice_a.exists(),
        "dead driver plus live guest group: slice survives"
    );
    assert!(
        lease_path(&session).exists(),
        "uncertain lease is not removed"
    );
    // Quiescence proven (guest group dead): orphan cleanup proceeds.
    // (Reap before the group poll: an unreaped SIGKILLed leader is a
    // zombie that still answers kill(-pgid, 0), so the poll would
    // never read dead.)
    kill_wait(guest_pid);
    let _ = hanging.wait();
    wait_group_dead(guest_pgid);
    let out = run_cistella(&home, &["gc"]);
    assert!(
        out.status.success(),
        "gc: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!slice_a.exists(), "post-quiescence: slice reaped");
    assert!(!lease_path(&session).exists(), "stale lease removed");
    assert!(slice_b.exists(), "independent live session untouched");
    // Release B: driver dead (killed below), no guest lines, no
    // unit — but first prove the guestless-lease skip is NOT what
    // happens here (B has a live driver until now).
    kill_wait(driver_b);
    let out = run_cistella(&home, &["gc"]);
    assert!(
        out.status.success(),
        "gc: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // B's lease names a dead driver with no guest lines: contradictory
    // provenance (no guest could have minted) — gc skips, slice stays.
    assert!(slice_b.exists(), "guestless dead-driver lease skips");
    std::fs::remove_dir_all(&slice_b).expect("remove fixture slice B");
    let _ = std::fs::remove_file(lease_path(&session_b));
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn tempdir_conduct_reaps_slice_on_teardown() {
    if !systemd_available() {
        eprintln!("skip: systemd user manager not available");
        return;
    }
    let worktree = src_worktree();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();
    let registry = write_tempdir_registry();
    let profile = fixture_profile("tempdir.toml");
    let (mut conduct, id, mut guard) = spawn_conduct_full(
        &home,
        &profile,
        &worktree_str,
        &[],
        &["sleep", "300"],
        &[(
            "XDG_CONFIG_HOME",
            registry.path().to_string_lossy().to_string().as_str(),
        )],
    );
    wait_active(&id);
    let container = format!("cistella-{id}");
    wait_harness(&container, "sleep 300");
    // The minted slice exists on the host during the session.
    let root = seat_root();
    let slices: Vec<_> = std::fs::read_dir(&root)
        .expect("read seat root")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .filter(|name| name.starts_with(&format!("cistella-tmp-{id}-")))
        .collect();
    assert_eq!(slices.len(), 1, "one session slice, got: {slices:?}");
    // Orderly terminate reaps the declared slice (teardown, not gc).
    let out = run_cistella(&home, &["terminate", &id]);
    assert!(
        out.status.success(),
        "terminate: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    guard.id = None;
    let _ = conduct.wait();
    assert!(
        !root.join(&slices[0]).exists(),
        "teardown reaps the declared slice"
    );
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn tempdir_natural_exit_reaps_slice() {
    if !systemd_available() {
        eprintln!("skip: systemd user manager not available");
        return;
    }
    let worktree = src_worktree();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();
    let registry = write_tempdir_registry();
    let profile = fixture_profile("tempdir.toml");
    // Short harness: the session completes naturally (no
    // terminate), exercising the teardown_unit clean path.
    let (mut conduct, id, mut guard) = spawn_conduct_full(
        &home,
        &profile,
        &worktree_str,
        &[],
        &["sleep", "2"],
        &[(
            "XDG_CONFIG_HOME",
            registry.path().to_string_lossy().to_string().as_str(),
        )],
    );
    wait_active(&id);
    // Pre-exit observation: the slice must exist mid-session.
    // Absence-only would pass even if nothing was ever minted;
    // the live-slice check proves the fixture exercises the reap
    // (same shape as the terminate test's mid-run assertion).
    let root = seat_root();
    let deadline = Instant::now() + Duration::from_secs(60);
    let slices = loop {
        let found: Vec<_> = std::fs::read_dir(&root)
            .expect("read seat root")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .filter(|name| name.starts_with(&format!("cistella-tmp-{id}-")))
            .collect();
        if !found.is_empty() {
            break found;
        }
        if Instant::now() >= deadline {
            panic!("session slice never minted for {id}");
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(slices.len(), 1, "one session slice, got: {slices:?}");
    let status = conduct.wait().expect("conduct completes");
    assert!(status.success(), "natural exit 0, got: {status:?}");
    guard.id = None;
    // Natural completion reaps the declared slice (same promise
    // as the terminate path — previously the clean path ran no
    // reap and the slice littered).
    let root = seat_root();
    let leftovers: Vec<_> = std::fs::read_dir(&root)
        .expect("read seat root")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .filter(|name| name.starts_with(&format!("cistella-tmp-{id}-")))
        .collect();
    assert!(leftovers.is_empty(), "no slice litter, got: {leftovers:?}");
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn tempdir_hooked_session_writes_tmp() {
    if !systemd_available() {
        eprintln!("skip: systemd user manager not available");
        return;
    }
    let worktree = src_worktree();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();
    let registry = write_tempdir_registry();
    let profile = fixture_profile("tempdir-hooked.toml");
    let (mut conduct, id, mut guard) = spawn_conduct_full(
        &home,
        &profile,
        &worktree_str,
        &[],
        &["sleep", "300"],
        &[(
            "XDG_CONFIG_HOME",
            registry.path().to_string_lossy().to_string().as_str(),
        )],
    );
    wait_active(&id);
    let container = format!("cistella-{id}");
    wait_harness(&container, "sleep 300");
    // Admit: the hooked harness writes inside the admitted /tmp.
    let out = Command::new("podman")
        .args(["exec", &container, "touch", "/tmp/admit-probe"])
        .output()
        .expect("podman exec");
    assert!(
        out.status.success(),
        "hooked /tmp write admits: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Nested outlet preserved: /tmp/scratch still writable beneath
    // the admitted slice (depth-ordered mounts, subtree handling).
    let out = Command::new("podman")
        .args(["exec", &container, "touch", "/tmp/scratch/nested-probe"])
        .output()
        .expect("podman exec");
    assert!(
        out.status.success(),
        "nested scratch writes admit: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = run_cistella(&home, &["terminate", &id]);
    assert!(
        out.status.success(),
        "terminate: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    guard.id = None;
    let _ = conduct.wait();
    // The slice is gone with the session (reaped, not litter).
    let root = seat_root();
    let leftovers: Vec<_> = std::fs::read_dir(&root)
        .expect("read seat root")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .filter(|name| name.starts_with(&format!("cistella-tmp-{id}-")))
        .collect();
    assert!(leftovers.is_empty(), "no slice litter, got: {leftovers:?}");
}
