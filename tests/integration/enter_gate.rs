//! Enter readiness-gate tests: the commit-event edge and its
//! refusals (Change 2 test surface).
//!
//! Split from `lifecycle.rs` at the file-size limit; the
//! terminate-while-attached conversion stays in place there,
//! gate refusal pins live here.

use std::process::Command;
use std::time::{Duration, Instant};

use super::helpers::*;

/// Spawn-time ownership for a gated conduct: the child, its
/// pipes, and its unit attribution are owned from spawn —
/// before any fallible id discovery — so every exit path
/// converges instead of stranding. Best-effort and never
/// panicking in Drop (report via eprintln); explicit paths
/// assert.
///
/// Attribution is always this invocation's worktree label
/// (never a before-set sweep): candidates are filtered by the
/// known set, then attributed by label, revalidated
/// immediately before any terminate. Ambiguity is reported,
/// never resolved by sweeping.
struct ParkedConduct {
    child: Option<std::process::Child>,
    home: String,
    worktree: String,
    unit_dir: std::path::PathBuf,
    before: std::collections::HashSet<String>,
    id: Option<String>,
    settled: bool,
}

impl ParkedConduct {
    /// Spawns a gated conduct (parked pre-initiate) and arms
    /// ownership immediately: the gate file is test-owned and
    /// absent (never pre-created — creation IS the release),
    /// and from here on the Drop owns converge-on-every-path.
    fn spawn(home: &str, worktree: &str, profile: &str, gate: &std::path::Path) -> Self {
        let unit_dir = std::path::PathBuf::from(home).join(".config/containers/systemd");
        let before: std::collections::HashSet<String> = std::fs::read_dir(&unit_dir)
            .map(|r| {
                r.flatten()
                    .map(|entry| entry.file_name().to_string_lossy().to_string())
                    .collect()
            })
            .unwrap_or_default();
        // Raw spawn (no id-line wait): the test learns the id
        // from the installed unit file, never from post-stall
        // stdout (which would race publication). Piped stdio
        // with no drain thread: the parked conduct writes
        // nothing before the test acts, and Drop drains what
        // little exists after the kill — bounded, never hanging.
        let child = std::process::Command::new(bin())
            .args([
                "conduct",
                "--profile",
                profile,
                "--session-directory",
                worktree,
                "--",
                "sleep",
                "300",
            ])
            .env("HOME", home)
            .env("TERM", "xterm-ghostty")
            .env("CISTELLA_CONDUCT_START_GATE", gate)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn gated conduct");
        Self {
            child: Some(child),
            home: home.to_string(),
            worktree: worktree.to_string(),
            unit_dir,
            before,
            id: None,
            settled: false,
        }
    }

    /// Discovers the installed unit's id (bounded; the timeout
    /// fails the test, never releases the conduct): the unit
    /// file lands at install, pre-gate, so this is where the
    /// pre-commit window starts.
    fn discover_id(&mut self) -> String {
        let (id, _) = wait_for_unit_file(&self.unit_dir, &self.before, &self.worktree);
        self.id = Some(id.clone());
        id
    }

    /// Unit ids attributable to this invocation: recorded unit
    /// files absent from the pre-spawn set whose directory
    /// label equals this invocation's worktree. The before-set
    /// is a candidate filter only — the label attributes.
    fn attributed_units(&self) -> Vec<String> {
        let mut found = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&self.unit_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if !name.starts_with("cistella-")
                    || !name.ends_with(".container")
                    || self.before.contains(&name)
                {
                    continue;
                }
                if unit_directory_label(&entry.path()) == self.worktree {
                    found.push(
                        name.trim_start_matches("cistella-")
                            .trim_end_matches(".container")
                            .to_string(),
                    );
                }
            }
        }
        found
    }

    /// Converges one unit by id (Drop-safe: reports, never
    /// panics). A `guard_terminate` `Ok` alone is not success:
    /// the helper returns `Ok` even for a nonzero CLI exit,
    /// so the exit status is inspected and stderr retained —
    /// a failed unit cleanup is reported, never silently
    /// accepted.
    fn converge_unit(&self, id: &str) {
        match guard_terminate(&self.home, id) {
            Ok(output) if output.status.success() => {}
            Ok(output) => {
                eprintln!(
                    "parked conduct teardown of {id} exited {}: {}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            Err(error) => {
                eprintln!("parked conduct teardown of {id} failed: {error:?}");
            }
        }
    }

    /// Aborts the parked conduct via SIGTERM and reaps the
    /// startup-abort disposition (explicit path: panics with
    /// context on failure, unlike Drop). The child stays owned
    /// throughout: a panic at any point unwinds into Drop,
    /// which still kills, reaps, drains, and converges.
    /// Never take it out for an explicit path.
    fn abort(&mut self) -> std::process::ExitStatus {
        let pid = self.child.as_ref().expect("conduct alive for abort").id();
        let mut kill = Command::new("kill");
        kill.args(["-TERM", &pid.to_string()]);
        run_bounded(kill, RUN_CISTELLA_TIMEOUT).expect("kill delivers");
        reap_bounded(
            self.child.as_mut().expect("conduct alive"),
            CHILD_REAP_TIMEOUT,
        )
        .expect("conduct reaped after abort")
    }

    /// Marks the session converged by an explicit path so
    /// Drop does not re-converge it (avoids noisy
    /// already-converged errors in the log).
    fn settle(&mut self) {
        self.settled = true;
    }
}

/// Bounded pipe drain: nonblocking reads until EOF, WouldBlock,
/// or a byte cap. Post-kill EOF is immediate (no writer
/// survives the reaped child); the cap bounds pathological
/// cases (a leaked writer elsewhere) instead of hanging.
fn drain_bounded<R: std::io::Read + std::os::unix::io::AsRawFd>(mut pipe: R, what: &str) {
    let fd = pipe.as_raw_fd();
    // Best effort: if nonblocking setup cannot be established,
    // close without reading rather than risk blocking forever
    // on a surviving writer. A blocking read with no
    // guaranteed EOF is never attempted here.
    let flags = unsafe { ::libc::fcntl(fd, ::libc::F_GETFL) };
    if flags < 0 {
        eprintln!("drain {what}: cannot query flags, closing unread");
        return;
    }
    if unsafe { ::libc::fcntl(fd, ::libc::F_SETFL, flags | ::libc::O_NONBLOCK) } < 0 {
        eprintln!("drain {what}: cannot set nonblocking, closing unread");
        return;
    }
    let mut total = 0usize;
    let mut buf = [0u8; 8192];
    loop {
        match std::io::Read::read(&mut pipe, &mut buf) {
            Ok(0) => break,
            Ok(n) => {
                total += n;
                if total > (1 << 20) {
                    eprintln!("drain {what}: exceeded 1MiB cap, stopping");
                    break;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(error) => {
                eprintln!("drain {what} failed: {error}");
                break;
            }
        }
    }
}

impl Drop for ParkedConduct {
    fn drop(&mut self) {
        // Child first: kill unblocks any gate/unit wait, reap
        // bounds the join, drains bound the pipes. No panics:
        // every fallible step reports via eprintln.
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            // reap_bounded returns None on timeout; try_wait
            // errors are equally terminal here. The child is
            // always owned at this point — explicit paths never
            // take it out — so this arm runs on every Drop.
            match reap_bounded(&mut child, CHILD_REAP_TIMEOUT) {
                Some(_) => {}
                None => eprintln!("parked conduct reap failed or timed out after kill"),
            }
            if let Some(out) = child.stdout.take() {
                drain_bounded(out, "stdout");
            }
            if let Some(err) = child.stderr.take() {
                drain_bounded(err, "stderr");
            }
        }
        if self.settled {
            return;
        }
        // Unit convergence by attribution: the known id if
        // discovered, else a worktree-label scan. Ambiguity is
        // reported, never resolved by sweeping.
        if let Some(id) = self.id.clone() {
            self.converge_unit(&id);
            return;
        }
        let mut candidates = self.attributed_units();
        // Re-check identity immediately before mutating: drop
        // anything that vanished since the scan.
        candidates.retain(|id| {
            unit_path(&self.home, id).exists() || {
                eprintln!("parked conduct candidate {id} vanished before converge; skipping");
                false
            }
        });
        match candidates.as_slice() {
            [] => {}
            [id] => self.converge_unit(id),
            many => eprintln!("parked conduct found ambiguous units, leaving untouched: {many:?}"),
        }
    }
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn conduct_enter_refuses_before_launch_commit() {
    // Negative control for the commit-event edge: a gated
    // conduct has installed its unit (record exists) but
    // cannot launch (no attestation ever commits), so `enter`
    // must refuse typed naming the missing attestation — never
    // exec into the unlaunched container. The create→initiate
    // boundary is held by an explicit test-controlled
    // handshake, not elapsed time: the conduct parks on the
    // gate file (no product-side timeout — absence of the file
    // never advances the session), so every assertion below
    // runs while commit is structurally unable to proceed.
    // ParkedConduct owns the child, pipes, and unit from spawn,
    // before any fallible id discovery; the test then aborts
    // via SIGTERM (128+SIGTERM, the startup-abort path).
    // Dropping that file here would be the release arm — this
    // test never creates it.
    if !systemd_gate() {
        return;
    }
    let worktree = src_worktree();
    std::fs::write(worktree.path().join("README.md"), "# test").unwrap();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();
    let profile = fixture_profile("default.toml");
    let gate_dir = tempfile::tempdir().expect("gate dir");
    let gate = gate_dir.path().join("start-gate");
    let mut parked = ParkedConduct::spawn(&home, &worktree_str, &profile, &gate);
    // Install is complete when discovery returns (bounded: its
    // timeout fails the test, never releases the conduct).
    let id = parked.discover_id();
    let base = cistella::lock::runtime_base();
    assert!(
        cistella::attestation::read(&base, &id)
            .expect("attestation read")
            .is_none(),
        "stalled conduct must not have committed"
    );
    let out = run_cistella(&home, &["enter", &id, "--", "true"]);
    assert!(
        !out.status.success(),
        "pre-commit enter must refuse, not exec"
    );
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        stderr.contains("no launch attestation"),
        "refusal names the missing attestation, got: {stderr}"
    );
    // Split contract: the same unattested session reports
    // successfully through `describe state` (observational,
    // exit 0) while `enter` refuses (nonzero above).
    let stated = run_cistella(&home, &["describe", "state", &id]);
    assert!(
        stated.status.success(),
        "describe state succeeds on unattested: {}",
        String::from_utf8_lossy(&stated.stderr)
    );
    let shown = String::from_utf8_lossy(&stated.stdout).to_string();
    assert!(
        shown.contains("commitment=unattested") && shown.contains("backend=unknown"),
        "describe reports unattested+unknown-backend, got: {shown}"
    );
    assert!(
        !shown.contains("state="),
        "no lifecycle snapshot without provenance, got: {shown}"
    );
    // Abort half of the handshake: SIGTERM the parked
    // conduct (the gate has no product-side timeout, so the
    // abort is the only way it advances) and reap the
    // startup-abort disposition (128+SIGTERM — the same path
    // the existing signal-during-startup test pins). The abort
    // tears down the installed unit; the id is disarmed
    // afterwards so Drop finds nothing left to converge.
    let status = parked.abort();
    assert_eq!(
        status.code(),
        Some(143),
        "abort disposition is 128+SIGTERM, got: {status:?}"
    );
    // The abort path converges the installed unit (terminate +
    // remove inside abort_startup): assert the teardown rather
    // than assuming it.
    assert!(
        !unit_path(&home, &id).exists(),
        "aborted startup leaves no unit file"
    );
    parked.settle();
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn conduct_parked_gate_aborts_on_pre_park_signal() {
    // Distinguishing ordering control for the check-then-park
    // question: the signal is delivered as early as causality
    // allows, so it may land before the first per-iteration
    // check or interrupt a parked poll — the outcome is
    // identical either way (abort, 143, unit converged),
    // because every path re-checks pending_signal and no path
    // advances on time. A blocking-open gate would park
    // forever in the pre-check-missed interleaving; this
    // polled gate cannot.
    //
    // Causality, not racing: the id is discovered first, which
    // requires the installed unit file — and signal handlers
    // are installed in main() long before create runs — so by
    // the time SIGTERM is sent, the disposition can only be
    // the abort path, never the pre-handler default. (Sending
    // before discovery could land before handler installation
    // and die by raw signal instead: a different, unasserted
    // path, and exactly the race this control refuses to run.)
    if !systemd_gate() {
        return;
    }
    let worktree = src_worktree();
    std::fs::write(worktree.path().join("README.md"), "# test").unwrap();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();
    let profile = fixture_profile("default.toml");
    let gate_dir = tempfile::tempdir().expect("gate dir");
    let gate = gate_dir.path().join("start-gate");
    let mut parked = ParkedConduct::spawn(&home, &worktree_str, &profile, &gate);
    // Boundary arrival first: the installed unit proves create
    // ran, which proves the handlers are installed. Only then
    // signal — the abort that follows is ordered, not raced.
    let id = parked.discover_id();
    let status = parked.abort();
    assert_eq!(
        status.code(),
        Some(143),
        "pre-park signal aborts with 128+SIGTERM, got: {status:?}"
    );
    assert!(
        !unit_path(&home, &id).exists(),
        "aborted startup leaves no unit file"
    );
    parked.settle();
}

/// Spawns a conduct, waits for its launch commitment, then kills
/// the owner: the session stays fully resourced (unit, running
/// container, attestation) with nobody to tear it down, so
/// out-of-band container surgery cannot race conduct
/// convergence. Returns the session triple with a guard armed
/// for panic-path teardown.
fn ownerless_committed_session(home: &str, worktree_str: &str) -> (String, String, Guard) {
    let profile = fixture_profile("default.toml");
    let (mut conduct, id, guard) = spawn_conduct_full(
        home,
        &profile,
        worktree_str,
        &[],
        &["sleep", "300"],
        &[],
        None,
    );
    let container = format!("cistella-{id}");
    let attested = wait_attestation(&id, &container);
    assert_eq!(
        attested.container_name, container,
        "attestation binds this session's container"
    );
    // Ownerless from here: the harness outlives its driver, and
    // the guard owns teardown on every path below.
    let _ = conduct.kill();
    let _ = reap_bounded(&mut conduct, CHILD_REAP_TIMEOUT);
    (id, container, guard)
}

/// Reads the live container ID via the same inspect template
/// the attestation binds. Returns None when the container is
/// transiently uninspectable (remove/recreate window) — the
/// caller keeps polling and fails loud only on the bound, never
/// mistaking a mid-swap absence for a reading.
fn live_container_id(container: &str) -> Option<String> {
    let out = run_bounded(
        {
            let mut cmd = Command::new("podman");
            cmd.args(["inspect", "--format", "{{.Id}}", container]);
            cmd
        },
        RUN_CISTELLA_TIMEOUT,
    )
    .expect("podman inspect runs");
    if !out.status.success() {
        return None;
    }
    let id = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if id.is_empty() {
        return None;
    }
    Some(id)
}

/// Reads the live container StartedAt via the bound template.
/// None on transient failure — same contract as
/// `live_container_id`.
fn live_started_at(container: &str) -> Option<String> {
    let out = run_bounded(
        {
            let mut cmd = Command::new("podman");
            cmd.args(["inspect", "--format", "{{.State.StartedAt}}", container]);
            cmd
        },
        RUN_CISTELLA_TIMEOUT,
    )
    .expect("podman inspect runs");
    if !out.status.success() {
        return None;
    }
    let started = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if started.is_empty() {
        return None;
    }
    Some(started)
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn conduct_enter_refuses_stale_incarnation_after_restart() {
    // Restart changes StartedAt under a stable name: the
    // pre-restart commitment must not ready the new
    // incarnation. Ownerless setup (helper above) removes the
    // conduct-teardown race — the only writer is the restart.
    if !systemd_gate() {
        return;
    }
    let worktree = src_worktree();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();
    let (id, container, mut guard) = ownerless_committed_session(&home, &worktree_str);
    let base = cistella::lock::runtime_base();
    let before = cistella::attestation::read(&base, &id)
        .expect("attestation reads")
        .expect("committed");
    // Sanity: the edge passes while the incarnation is current
    // (ownerless direct exec of a no-op proves ProceedNative).
    let fresh = run_cistella(&home, &["enter", &id, "--", "true"]);
    assert!(
        fresh.status.success(),
        "edge passes pre-restart: {}",
        String::from_utf8_lossy(&fresh.stderr)
    );
    let out = run_bounded(
        {
            let mut cmd = Command::new("podman");
            cmd.args(["restart", &container]);
            cmd
        },
        RUN_CISTELLA_TIMEOUT,
    )
    .expect("podman restart runs");
    assert!(
        out.status.success(),
        "podman restart: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Bounded wait for the new incarnation to settle (fail loud
    // on a restart that never lands — never assume the swap).
    // An out-of-band `podman restart` under systemd returns
    // while the container is still transitioning, so the first
    // differing StartedAt may precede a window where the next
    // inspect fails — which the single-pass enter gate reports
    // as unestablishable, not stale. Only consecutive identical
    // identity readings (same Id and StartedAt, StartedAt
    // differing from the bound) prove the backend quiescent;
    // transient failures reset the streak, never read as absence.
    let bound_started = before
        .incarnation
        .as_ref()
        .expect("native commitment carries incarnation")
        .started_at
        .clone();
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut streak: u32 = 0;
    let mut last: Option<(String, String)> = None;
    loop {
        let current = match (live_container_id(&container), live_started_at(&container)) {
            (Some(id), Some(started)) if started != bound_started => Some((id, started)),
            _ => None,
        };
        if current.is_some() && current == last {
            streak += 1;
            if streak >= 3 {
                break;
            }
        } else {
            streak = u32::from(current.is_some());
            last = current;
        }
        if Instant::now() > deadline {
            panic!("restart never settled a new incarnation for {container}");
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    let out = run_cistella(&home, &["enter", &id, "--", "true"]);
    assert!(
        !out.status.success(),
        "post-restart enter must refuse, not exec"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("stale"),
        "refusal names staleness, got: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let term = run_cistella(&home, &["terminate", &id]);
    assert!(
        term.status.success(),
        "restarted unit terminates: {}",
        String::from_utf8_lossy(&term.stderr)
    );
    guard.id = None;
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn conduct_enter_after_teardown_reports_selector_not_readiness() {
    // Deletion-ordering pin: teardown removes the unit file
    // before reaping the attestation, so a post-teardown enter
    // resolves to the Selector no-match — never a stale
    // readiness claim from residue, and never an `absent`
    // fabrication.
    if !systemd_gate() {
        return;
    }
    let worktree = src_worktree();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();
    let (id, container, mut guard) = ownerless_committed_session(&home, &worktree_str);
    let _ = container;
    let term = run_cistella(&home, &["terminate", &id]);
    assert!(
        term.status.success(),
        "terminate converges: {}",
        String::from_utf8_lossy(&term.stderr)
    );
    guard.id = None;
    let out = run_cistella(&home, &["enter", &id, "--", "true"]);
    assert!(!out.status.success(), "post-teardown enter must fail");
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        stderr.contains("no session matches"),
        "record-gone dominates as Selector, got: {stderr}"
    );
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn conduct_enter_refuses_stale_incarnation_after_recreate() {
    // Out-of-band rm under Restart=on-failure: systemd recreates
    // the same name with a NEW container ID. The old commitment
    // must not ready the replacement. Same ownerless shape as
    // the restart test (ID change instead of StartedAt).
    if !systemd_gate() {
        return;
    }
    let worktree = src_worktree();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();
    let (id, container, mut guard) = ownerless_committed_session(&home, &worktree_str);
    let base = cistella::lock::runtime_base();
    let before = cistella::attestation::read(&base, &id)
        .expect("attestation reads")
        .expect("committed");
    let bound_id = before
        .incarnation
        .as_ref()
        .expect("native commitment carries incarnation")
        .container_id
        .clone();
    // Pre-surgery identity, tolerantly: a transient inspect
    // failure here must retry, not fail — the loop below
    // already owns the fail-loud bound, this just establishes
    // the baseline it compares against.
    let deadline = Instant::now() + Duration::from_secs(60);
    let bound_live = loop {
        if let Some(current) = live_container_id(&container) {
            break current;
        }
        if Instant::now() > deadline {
            panic!("container never inspectable pre-surgery: {container}");
        }
        std::thread::sleep(Duration::from_millis(500));
    };
    assert_eq!(
        bound_live, bound_id,
        "attested ID matches the live container pre-surgery"
    );
    // Failure-inducing surgery: `podman kill` (SIGKILL) exits
    // the container as a failure, so the unit's
    // Restart=on-failure fires and systemd recreates the same
    // name with a new container ID. (`rm -f` exits clean and
    // never recreates — the wait below would be unreachable.)
    let out = run_bounded(
        {
            let mut cmd = Command::new("podman");
            cmd.args(["kill", &container]);
            cmd
        },
        RUN_CISTELLA_TIMEOUT,
    )
    .expect("podman kill runs");
    assert!(
        out.status.success(),
        "podman kill: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Bounded wait for systemd's replacement (fail loud if the
    // service never recreates — never assume the swap).
    // Transient inspect failures (kill/recreate window) read as
    // not-yet: only a successful query naming a different ID
    // breaks the loop.
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match live_container_id(&container) {
            Some(current) if current != bound_id => break,
            _ => {}
        }
        if Instant::now() > deadline {
            panic!("recreate never landed a new ID for {container}");
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    let out = run_cistella(&home, &["enter", &id, "--", "true"]);
    assert!(
        !out.status.success(),
        "post-recreate enter must refuse, not exec"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("stale"),
        "refusal names staleness, got: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let term = run_cistella(&home, &["terminate", &id]);
    assert!(
        term.status.success(),
        "recreated unit terminates: {}",
        String::from_utf8_lossy(&term.stderr)
    );
    guard.id = None;
}

#[ignore = "live: requires systemd user manager and podman"]
#[test]
fn conduct_gate_release_proceeds_to_commit() {
    // Positive control for the release gate: creating the
    // release file lets the parked conduct proceed to initiate,
    // launch, and commit. Proves the gate does not wedge a
    // normal launch — the release branch is taken, not just
    // present. The abort path (refusal tests above) covers the
    // other arm.
    if !systemd_gate() {
        return;
    }
    let worktree = src_worktree();
    std::fs::write(worktree.path().join("README.md"), "# test").unwrap();
    let worktree_str = worktree.path().to_string_lossy().to_string();
    let home = home_dir();
    let profile = fixture_profile("default.toml");
    let gate_dir = tempfile::tempdir().expect("gate dir");
    let gate = gate_dir.path().join("start-gate");
    let mut parked = ParkedConduct::spawn(&home, &worktree_str, &profile, &gate);
    let id = parked.discover_id();
    let container = format!("cistella-{id}");
    // Release: create the gate file; the next poll iteration
    // observes it and the conduct proceeds. The attestation
    // must then commit — proving the gate released rather
    // than wedged.
    std::fs::write(&gate, b"release").expect("release the gate");
    let attested = wait_attestation(&id, &container);
    assert_eq!(
        attested.container_name, container,
        "released conduct commits its launch"
    );
    // Normal teardown from here: terminate converges the
    // launched session, reap the conduct, then confirm the
    // unit is gone before disarming (Drop would converge it
    // identically, but the assertion belongs to the test).
    let term = run_cistella(&home, &["terminate", &id]);
    assert!(
        term.status.success(),
        "released session terminates: {}",
        String::from_utf8_lossy(&term.stderr)
    );
    let status = reap_bounded(
        parked.child.as_mut().expect("conduct alive for reap"),
        CHILD_REAP_TIMEOUT,
    )
    .expect("conduct reaped after teardown");
    eprintln!("conduct end: {status:?}");
    assert!(
        !unit_path(&home, &id).exists(),
        "released session leaves no unit file"
    );
    parked.settle();
}
