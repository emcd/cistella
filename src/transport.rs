//! Transport: host PTY owns session, exec -i -t, closed env, resize.

use crate::error::{CistellaError, Result};

/// Closed env list forwarded via `-e`. `TERMINFO` is never forwarded with
/// baked images.
pub const CLOSED_ENV: &[&str] = &["TERM", "COLORTERM", "TERM_PROGRAM"];

/// Returns `-e` args for the closed env list from the current host env.
///
/// Only present vars are forwarded; absent vars are omitted. `TERMINFO` is
/// explicitly not forwarded.
#[must_use]
pub fn env_forward_args() -> Vec<String> {
    let mut args = Vec::new();
    for key in CLOSED_ENV {
        if let Ok(val) = std::env::var(key)
            && !val.is_empty()
        {
            args.push("-e".to_string());
            args.push(format!("{key}={val}"));
        }
    }
    args
}

/// Returns home env arg derived from the profile's container_home.
#[must_use]
pub fn home_env_arg(container_home: &str) -> Vec<String> {
    vec!["-e".to_string(), format!("HOME={container_home}")]
}

/// Builds `podman exec -i -t` args. Stdio must be on the session PTY slave.
///
/// The closed env `TERM`/`COLORTERM`/`TERM_PROGRAM` is forwarded at exec
/// time via `-e` (transport spec), not baked into the unit; `HOME` is
/// static from the profile's `container_home` and lives in the Quadlet unit.
#[must_use]
pub fn exec_args(container: &str, command: &[String]) -> Vec<String> {
    let mut args = vec!["exec".to_string()];
    args.extend(env_forward_args());
    args.extend(["-i".to_string(), "-t".to_string(), container.to_string()]);
    args.extend(command.iter().cloned());
    args
}

/// Builds `podman exec -i -t --workdir <target>` args for the conduct
/// harness: the session runs in its worktree target, not the image
/// default. Companion `enter` keeps plain [`exec_args`] (operator's
/// shell, operator's cwd).
#[must_use]
pub fn exec_harness_args(container: &str, workdir: &str, command: &[String]) -> Vec<String> {
    let mut args = vec!["exec".to_string()];
    args.extend(env_forward_args());
    args.extend([
        "-i".to_string(),
        "-t".to_string(),
        "--workdir".to_string(),
        workdir.to_string(),
        container.to_string(),
    ]);
    args.extend(command.iter().cloned());
    args
}

/// Whether `podman exec --help` text advertises the singular
/// `--preserve-fd` list form: some line must DEFINE it —
/// leading whitespace, then exactly `--preserve-fd`, then an
/// option-field delimiter (end of line, space, tab, `=`, `,`).
/// A bare token anywhere is not enough: prose merely mentioning
/// the unsupported option ("this build does not support
/// `--preserve-fd`") must not satisfy a pre-spawn capability
/// gate, and the plural `--preserve-fds` count form never
/// satisfies it (sibling-session fd leak). Pure over the help
/// text; the runner below supplies it.
///
/// The singular form is load-bearing for hooked launches: the
/// range form would forward every guest-held fd in range —
/// including sibling-session descriptors — into the container.
/// Old podman (4.9.x) knows only the plural form, so hooked
/// launches carry a podman floor enforced by the runner.
#[must_use]
pub fn exec_help_supports_preserve_fd(help: &str) -> bool {
    help.lines().any(|line| {
        line.trim_start()
            .strip_prefix("--preserve-fd")
            .is_some_and(|rest| matches!(rest.chars().next(), None | Some(' ' | '\t' | '=' | ',')))
    })
}

/// Refuses hooked launches the seat podman cannot forward:
/// `podman exec` must advertise the singular `--preserve-fd`
/// (exact-fd list, crun runtime). Old podman refuses typed
/// pre-exec — never a flag-parse death misread as wrapper
/// failure downstream.
///
/// # Errors
///
/// Returns `CistellaError::Runtime` when podman cannot run and
/// `CistellaError::Contract` when the flag is absent.
pub fn check_podman_preserve_fd() -> Result<()> {
    let output = std::process::Command::new("podman")
        .args(["exec", "--help"])
        .output()
        .map_err(|e| CistellaError::Runtime(format!("podman exec --help: {e}")))?;
    if !output.status.success() {
        return Err(CistellaError::Runtime(
            "podman exec --help failed".to_string(),
        ));
    }
    let help = String::from_utf8_lossy(&output.stdout);
    if !exec_help_supports_preserve_fd(&help) {
        return Err(CistellaError::Contract(
            "podman exec lacks --preserve-fd: hooked launch needs podman with singular --preserve-fd plus the crun runtime"
                .to_string(),
        ));
    }
    Ok(())
}

/// Diagnostics fd number inside the container on the plural
/// path: the pre-exec dup collapses the write-end onto 3 so
/// `--preserve-fds=1` forwards exactly `{0,1,2,3}` (proven
/// contiguous `[3,3+N)` semantics: any gap fails closed).
pub const PLURAL_DIAG_FD: std::os::fd::RawFd = 3;

/// Preservation strategy for one hooked launch, resolved from
/// the seat podman (stock-24.04 goal: no non-distro
/// requirement for confinement).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreserveStrategy {
    /// Exact list form (podman advertising singular
    /// `--preserve-fd`, crun runtime): forwards only the named
    /// fd at its natural number. Preferred wherever available
    /// — robust regardless of the guest fd table.
    Singular,
    /// Count form (older podman): `--preserve-fds=1` with the
    /// write-end duped onto [`PLURAL_DIAG_FD`] pre-exec, plus a
    /// per-launch inheritable check on the diagnostics fd
    /// itself. Safe by dup+N mechanics (container-visible set
    /// exactly `{0,1,2,3}`): ambient inheritable fds stay
    /// podman-side-only.
    Plural,
}

/// Resolves the preservation strategy: singular where the seat
/// podman advertises it, plural otherwise (the plural count
/// form is ancient — present wherever hooked launches run
/// locally). A podman that cannot even run `--help` fails
/// Runtime (both paths need podman); a missing singular flag
/// is not an error here, it selects the plural path.
///
/// # Errors
///
/// Returns `CistellaError::Runtime` when podman cannot run.
pub fn resolve_preserve_strategy() -> Result<PreserveStrategy> {
    match check_podman_preserve_fd() {
        Ok(()) => Ok(PreserveStrategy::Singular),
        Err(CistellaError::Contract(_)) => Ok(PreserveStrategy::Plural),
        Err(other) => Err(other),
    }
}

/// Asserts the diagnostics fd itself is inheritable (plural-
/// and singular-path precondition, every launch): the podman
/// child must carry it, or preservation names a closed fd
/// (Q1's contiguous gate fails closed, exit 125). Checks only
/// OUR fd — never the ambient table: with the pre-exec dup
/// onto [`PLURAL_DIAG_FD`] plus `--preserve-fds=1`, the
/// container-visible set is exactly `{0,1,2,3}` by dup+N
/// mechanics and Q1 evidence, so ambient inheritable fds stay
/// podman-side-only (Q3 confirms the arriving table). A
/// whole-table census would refuse on invoker plumbing (test
/// workers, shells, agentmux pipes — proven live) without
/// buying container-side safety. Pinned directly with owned
/// pipes (no table dependence).
///
/// # Errors
///
/// Returns `CistellaError::Contract` when the fd carries
/// CLOEXEC and `CistellaError::Runtime` on fcntl failure.
pub fn assert_diag_inheritable(diag: std::os::fd::RawFd) -> Result<()> {
    let flags = nix::fcntl::fcntl(diag, nix::fcntl::FcntlArg::F_GETFD)
        .map_err(|e| CistellaError::Runtime(format!("diagnostics fcntl: {e}")))?;
    if nix::fcntl::FdFlag::from_bits_retain(flags) & nix::fcntl::FdFlag::FD_CLOEXEC
        != nix::fcntl::FdFlag::empty()
    {
        return Err(CistellaError::Contract(format!(
            "diagnostics fd {diag} not inheritable at hooked spawn: preservation refused"
        )));
    }
    Ok(())
}

/// Builds hooked-launch exec args without a workdir override:
/// as [`exec_args`] plus the preservation flag for the
/// strategy (exact singular list form, or the plural count
/// form with the write-end pre-duped onto [`PLURAL_DIAG_FD`]).
#[must_use]
pub fn exec_hooked_plain_args(
    container: &str,
    command: &[String],
    strategy: PreserveStrategy,
    preserve_fd: std::os::fd::RawFd,
) -> Vec<String> {
    let mut args = vec!["exec".to_string()];
    args.extend(env_forward_args());
    args.extend([
        "-i".to_string(),
        "-t".to_string(),
        match strategy {
            PreserveStrategy::Singular => format!("--preserve-fd={preserve_fd}"),
            PreserveStrategy::Plural => "--preserve-fds=1".to_string(),
        },
        container.to_string(),
    ]);
    args.extend(command.iter().cloned());
    args
}
/// Builds hooked-launch exec args with a workdir override:
/// exact `--preserve-fd={fd}` for the diagnostics write-end on
/// the singular path; `--preserve-fds=1` (write-end pre-duped
/// onto [`PLURAL_DIAG_FD`]) on the plural path. The singular
/// list form forwards only the named fd; the plural count form
/// forwards exactly `{0,1,2,3}` by dup+N mechanics (ambient
/// inheritable fds stay podman-side-only — the per-launch
/// check covers the diagnostics fd itself).
/// Crun-or-runc per the strategy matrix; the backend refuses
/// unadmitted runtimes typed before spawn.
#[must_use]
pub fn exec_hooked_args(
    container: &str,
    workdir: &str,
    command: &[String],
    strategy: PreserveStrategy,
    preserve_fd: std::os::fd::RawFd,
) -> Vec<String> {
    let mut args = vec!["exec".to_string()];
    args.extend(env_forward_args());
    args.extend([
        "-i".to_string(),
        "-t".to_string(),
        match strategy {
            PreserveStrategy::Singular => format!("--preserve-fd={preserve_fd}"),
            PreserveStrategy::Plural => "--preserve-fds=1".to_string(),
        },
        "--workdir".to_string(),
        workdir.to_string(),
        container.to_string(),
    ]);
    args.extend(command.iter().cloned());
    args
}

/// Builds `podman run -d --userns=keep-id --label ...` args for the spike.
///
/// Driver's `run` generates a Quadlet unit instead; this helper is for the
/// spike and direct `podman run -d` path.
#[must_use]
pub fn run_detached_args(
    container_name: &str,
    labels: &[(&str, &str)],
    image: &str,
) -> Vec<String> {
    let mut args = vec![
        "run".to_string(),
        "--detach".to_string(),
        "--userns=keep-id".to_string(),
        "--name".to_string(),
        container_name.to_string(),
    ];
    for (k, v) in labels {
        args.push("--label".to_string());
        args.push(format!("{k}={v}"));
    }
    args.extend(env_forward_args());
    args.push("--".to_string());
    args.push(image.to_string());
    args.push("sleep".to_string());
    args.push("infinity".to_string());
    args
}
