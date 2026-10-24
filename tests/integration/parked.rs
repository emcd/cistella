//! Spawn-time owned cleanup for gated conducts: the child, its
//! pipes, and its unit attribution are owned from spawn —
//! before any fallible id discovery — so every exit path
//! converges instead of stranding. Best-effort and never
//! panicking in Drop (report via eprintln); explicit paths
//! assert.
//!
//! Attribution is always this invocation's worktree label
//! (never a before-set sweep): candidates are filtered by the
//! known set, then attributed by label, revalidated
//! immediately before any terminate. Ambiguity is reported,
//! never resolved by sweeping.

use std::process::Command;

use super::helpers::*;

pub struct ParkedConduct {
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
    pub fn spawn(home: &str, worktree: &str, profile: &str, gate: &std::path::Path) -> Self {
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
    pub fn discover_id(&mut self) -> String {
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
    pub fn abort(&mut self) -> std::process::ExitStatus {
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
    pub fn settle(&mut self) {
        self.settled = true;
    }

    /// Reaps the owned child after an explicit-path teardown
    /// (the child stays owned throughout: a panic at any
    /// point unwinds into Drop, which still converges).
    /// Panics with context on failure, unlike Drop.
    pub fn reap_child(&mut self) -> std::process::ExitStatus {
        reap_bounded(
            self.child.as_mut().expect("conduct alive for reap"),
            CHILD_REAP_TIMEOUT,
        )
        .expect("conduct reaped after teardown")
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
