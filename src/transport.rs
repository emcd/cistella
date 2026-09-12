//! Transport: host PTY owns session, exec -i -t, closed env, resize.

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
