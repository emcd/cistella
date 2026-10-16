//! Tmpdir service live proofs: gc lease discipline, teardown
//! reap, and hooked `/tmp` admission through production conduct.
//!
//! The gc tests drive the real `gc` binary against hand-built
//! leases, slices, and guest processes (no conduct orchestration
//! needed for the lease logic); the conduct tests run full
//! sessions with the shipped tmpdir extension (no registry —
//! shipped resolution, the registry stays third-party-only).

use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::time::{Duration, Instant};

use super::helpers::*;

/// Spawns a hanging fake_guest (hello, then sleeps) in its own
/// process group: stands in for a paused pre-response prepare
/// guest. The caller owns quiescence (kill, then wait).
fn spawn_hanging_guest() -> std::process::Child {
    use std::os::unix::process::CommandExt;
    let mut child = Command::new(example_binary("fake_guest"));
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
    let mut hanging = spawn_hanging_guest();
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
    let _ = reap_bounded(&mut driver_a, CHILD_REAP_TIMEOUT);
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
    let _ = reap_bounded(&mut hanging, CHILD_REAP_TIMEOUT);
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
fn tmpdir_conduct_reaps_slice_on_teardown() {
    if !systemd_available() {
        skip("systemd user manager not available");
        return;
    }
    let worktree = src_worktree();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();
    let profile = fixture_profile("tmpdir.toml");
    let (mut conduct, id, mut guard) =
        spawn_conduct_full(&home, &profile, &worktree_str, &[], &["sleep", "300"], &[]);
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
    let _ = reap_bounded(&mut conduct, CHILD_REAP_TIMEOUT);
    assert!(
        !root.join(&slices[0]).exists(),
        "teardown reaps the declared slice"
    );
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn tmpdir_natural_exit_reaps_slice() {
    if !systemd_available() {
        skip("systemd user manager not available");
        return;
    }
    let worktree = src_worktree();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();
    let profile = fixture_profile("tmpdir.toml");
    // Short harness: the session completes naturally (no
    // terminate), exercising the teardown_unit clean path.
    let (mut conduct, id, mut guard) =
        spawn_conduct_full(&home, &profile, &worktree_str, &[], &["sleep", "2"], &[]);
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
    let status = reap_bounded(&mut conduct, CHILD_REAP_TIMEOUT).expect("conduct completes");
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
fn tmpdir_hooked_session_writes_tmp() {
    if !systemd_available() {
        skip("systemd user manager not available");
        return;
    }
    let worktree = src_worktree();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();
    let profile = fixture_profile("tmpdir-hooked.toml");
    // Wrapped-lineage admission: the harness itself writes
    // inside the admitted /tmp (podman exec runs outside the
    // guarantee and proves only plumbing). The host-side probe
    // file proves the confined write landed.
    let (mut conduct, id, mut guard) = spawn_conduct_full(
        &home,
        &profile,
        &worktree_str,
        &[],
        &["sh", "-c", "touch /tmp/admit-probe && sleep 300"],
        &[],
    );
    wait_active(&id);
    let container = format!("cistella-{id}");
    wait_harness(&container, "sleep 300");
    let root = seat_root();
    let slices: Vec<_> = std::fs::read_dir(&root)
        .expect("read seat root")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .filter(|name| name.starts_with(&format!("cistella-tmp-{id}-")))
        .collect();
    assert_eq!(slices.len(), 1, "one session slice, got: {slices:?}");
    assert!(
        root.join(&slices[0]).join("admit-probe").exists(),
        "wrapped harness wrote inside the admitted slice"
    );
    let out = run_cistella(&home, &["terminate", &id]);
    assert!(
        out.status.success(),
        "terminate: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    guard.id = None;
    let _ = reap_bounded(&mut conduct, CHILD_REAP_TIMEOUT);
    // The slice is gone with the session (reaped, not litter).
    let leftovers: Vec<_> = std::fs::read_dir(&root)
        .expect("read seat root")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .filter(|name| name.starts_with(&format!("cistella-tmp-{id}-")))
        .collect();
    assert!(leftovers.is_empty(), "no slice litter, got: {leftovers:?}");
}

/// Writes a minimal hooked profile (landlock + ~/src RO plus
/// caller-supplied extra mount TOML) for admission/retention
/// proofs that need no tmpdir extension.
fn write_hooked_profile(dir: &std::path::Path, extra_mounts: &str) -> String {
    let text = format!(
        "image = 'localhost/cistella/opencode:example'\n\
         container-home = '/home/cistella'\n\
         command = ['sleep', 'infinity']\n\
         [[mounts]]\n\
         host-source = '~/src'\n\
         container-target = '{{{{core:container-home}}}}/src'\n\
         mode = 'ro'\n\
         {extra_mounts}\
         [[extensions]]\n\
         name = 'landlock'\n"
    );
    let path = dir.join("hooked-test.toml");
    std::fs::write(&path, text).expect("write profile");
    path.to_string_lossy().to_string()
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn hooked_tmp_admits_through_wrapped_harness() {
    if !systemd_available() {
        skip("systemd user manager not available");
        return;
    }
    // Unconditional baseline proof through the WRAPPED harness
    // lineage (not podman exec, which runs outside the
    // guarantee): the harness itself writes /tmp and exits
    // cleanly — natural exit 0 proves admission.
    let worktree = src_worktree();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();
    let profdir = tempfile::tempdir().expect("profile dir");
    let profile = write_hooked_profile(profdir.path(), "");
    let (mut conduct, _id, mut guard) = spawn_conduct_full(
        &home,
        &profile,
        &worktree_str,
        &[],
        &["sh", "-c", "echo admitted > /tmp/admit-probe && sleep 5"],
        &[],
    );
    let status = reap_bounded(&mut conduct, CHILD_REAP_TIMEOUT).expect("conduct completes");
    assert!(
        status.success(),
        "wrapped /tmp write admits, got: {status:?}"
    );
    guard.id = None;
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn hooked_ro_mirror_denies_through_wrapped_harness() {
    if !systemd_available() {
        skip("systemd user manager not available");
        return;
    }
    // Retention proof: a static source bound RO at /tmp (the
    // baseline FULL roots its dentries) and RO at /mirror keeps
    // VFS read-only on both routes — the wrapped harness cannot
    // write through the alias and the host source is unmodified.
    let worktree = src_worktree();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();
    let shared = tempfile::tempdir().expect("shared source");
    let shared_str = shared.path().to_string_lossy().to_string();
    std::fs::write(shared.path().join("victim"), "precious").expect("victim");
    let extra = format!(
        "[[mounts]]\nhost-source = '{shared_str}'\ncontainer-target = '/tmp'\nmode = 'ro'\n\
         [[mounts]]\nhost-source = '{shared_str}'\ncontainer-target = '/mirror'\nmode = 'ro'\n"
    );
    let profdir = tempfile::tempdir().expect("profile dir");
    let profile = write_hooked_profile(profdir.path(), &extra);
    // Single wrapped harness, exit 0 ONLY on (read-ok AND
    // write-denied): exit 11 = alias not materialized, exit 7 =
    // write wrongly admitted, exit 0 = read succeeded and the
    // VFS read-only denial observed. Separate conducts would
    // let any failed session masquerade as the denial.
    let (mut conduct, _id, mut guard) = spawn_conduct_full(
        &home,
        &profile,
        &worktree_str,
        &[],
        &[
            "sh",
            "-c",
            "[ \"$(cat /mirror/victim)\" = precious ] || exit 11; \
             if echo intruder > /mirror/probe 2>/dev/null; then exit 7; else exit 0; fi",
        ],
        &[],
    );
    let status = reap_bounded(&mut conduct, CHILD_REAP_TIMEOUT).expect("conduct completes");
    assert!(
        status.success(),
        "wrapped read-ok plus write-denied must exit 0, got: {status:?}"
    );
    guard.id = None;
    assert_eq!(
        std::fs::read(shared.path().join("victim")).expect("victim intact"),
        b"precious"
    );
    assert!(
        !shared.path().join("probe").exists(),
        "no alias write landed"
    );
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn hooked_ancestor_alias_refuses_precreate() {
    if !systemd_available() {
        skip("systemd user manager not available");
        return;
    }
    // Ancestor-covered baseline alias: a sibling source bound RO
    // at /tmp sits under the RO ~/src ancestor, which flips to
    // Podman RW for materialization — contradictory topology
    // refuses pre-create typed (no session starts).
    let worktree = src_worktree();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();
    let sib = tempfile::TempDir::new_in(std::path::Path::new(&home).join("src"))
        .expect("sibling under ancestor");
    let sib_str = sib.path().to_string_lossy().to_string();
    let extra =
        format!("[[mounts]]\nhost-source = '{sib_str}'\ncontainer-target = '/tmp'\nmode = 'ro'\n");
    let profdir = tempfile::tempdir().expect("profile dir");
    let profile = write_hooked_profile(profdir.path(), &extra);
    // Refused conducts never print an id line (no session
    // starts): invoke directly and assert exit + message
    // instead of waiting on spawn_conduct_full's id read.
    let out = run_cistella(
        &home,
        &[
            "conduct",
            "--profile",
            profile.as_str(),
            "--session-directory",
            worktree_str.as_str(),
            "--",
            "sleep",
            "300",
        ],
    );
    assert!(
        !out.status.success(),
        "ancestor-covered alias must refuse pre-create, got: {}",
        out.status
    );
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        stderr.contains("aliases FULL-backed /tmp source"),
        "refusal names the alias contract, got: {stderr}"
    );
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn tmpdir_nested_user_bind_reaps_with_slice() {
    if !systemd_available() {
        skip("systemd user manager not available");
        return;
    }
    // Generic pre-create live: a user bind nested under the
    // slice gets its mountpoint seeded seat-owned (not
    // container-root), writes through, and reaps with the root
    // on orderly terminate.
    let worktree = src_worktree();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();
    let cache_src = tempfile::tempdir().expect("cache source");
    let cache_str = cache_src.path().to_string_lossy().to_string();
    let profile = fixture_profile("tmpdir.toml");
    let (mut conduct, id, mut guard) = spawn_conduct_full(
        &home,
        &profile,
        &worktree_str,
        &["--mount", &format!("{cache_str}:/tmp/cache:rw")],
        &["sleep", "300"],
        &[],
    );
    wait_active(&id);
    let container = format!("cistella-{id}");
    wait_harness(&container, "sleep 300");
    let mut cmd = Command::new("podman");
    cmd.args(["exec", &container, "touch", "/tmp/cache/nested-probe"]);
    let out = run_bounded(cmd, RUN_CISTELLA_TIMEOUT).expect("podman exec");
    assert!(
        out.status.success(),
        "nested user bind writes admit: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = run_cistella(&home, &["terminate", &id]);
    assert!(
        out.status.success(),
        "terminate: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    guard.id = None;
    let _ = reap_bounded(&mut conduct, CHILD_REAP_TIMEOUT);
    let root = seat_root();
    let leftovers: Vec<_> = std::fs::read_dir(&root)
        .expect("read seat root")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .filter(|name| name.starts_with(&format!("cistella-tmp-{id}-")))
        .collect();
    assert!(leftovers.is_empty(), "no slice litter, got: {leftovers:?}");
}
