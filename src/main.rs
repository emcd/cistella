//! Cistella CLI entry point.

use cistella::cli::{Cli, Command};
use cistella::mount::podman_volume_args;
use cistella::profile::Profile;
use cistella::runtime::{
    SessionId, gc_exited, generate_quadlet_unit, install_quadlet, logs_container, start_quadlet,
    stop_container,
};
use clap::Parser;

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::ExitCode::from(1)
        }
    }
}

fn run(cli: Cli) -> Result<(), cistella::error::CistellaError> {
    match cli.command {
        Command::Run {
            session_id,
            seat,
            harness,
            profile,
            worktree,
            profile_file,
            image,
        } => {
            let session = SessionId {
                session_id: session_id.clone(),
                seat: seat.clone(),
                harness: harness.clone(),
                profile: profile.clone(),
            };
            let mut prof = load_profile(profile_file.as_deref(), &harness)?;
            // Worktree is required and becomes an allowlist triple at /work
            let work_triple = cistella::mount::MountTriple {
                host_source: worktree,
                container_target: "/work".to_string(),
                mode: cistella::mount::MountMode::Rw,
            };
            // Per-session scratch
            let scratch_host = format!("/tmp/cistella-{session_id}");
            let _ = std::fs::create_dir_all(&scratch_host);
            let scratch_triple = cistella::mount::MountTriple {
                host_source: scratch_host,
                container_target: "/tmp/scratch".to_string(),
                mode: cistella::mount::MountMode::Rw,
            };
            prof.mounts.push(work_triple);
            prof.mounts.push(scratch_triple);
            cistella::mount::validate_mounts(&prof.mounts, prof.home())?;
            cistella::identity::assert_no_github_token(&prof)?;
            let volumes = podman_volume_args(&prof.mounts, prof.home(), None);
            let ssh_args = cistella::identity::ssh_agent_volume_args(&prof);
            // ssh_args is mixed ["--volume", "sock:sock:ro", "-e", "SSH_AUTH_SOCK=..."]; split for Quadlet
            let mut all_volumes = volumes;
            let mut env_extra: Vec<String> =
                prof.env.iter().map(|(k, v)| format!("{k}={v}")).collect();
            let mut i = 0;
            while i + 1 < ssh_args.len() {
                let flag = &ssh_args[i];
                let val = &ssh_args[i + 1];
                if flag == "--volume" {
                    all_volumes.push(flag.clone());
                    all_volumes.push(val.clone());
                } else if flag == "-e" {
                    env_extra.push(val.clone());
                }
                i += 2;
            }
            let unit =
                generate_quadlet_unit(&session, &image, &all_volumes, &env_extra, prof.home())?;
            let unit_name = session.quadlet_unit_name();
            install_quadlet(&unit_name, &unit)?;
            start_quadlet(&unit_name)?;
            println!("started {}", session.container_name());
        }
        Command::Stop {
            session_id,
            harness,
        } => {
            let name = format!("cistella-{harness}-{session_id}");
            stop_container(&name)?;
            // Clean per-session scratch using the authoritative session_id (avoid lossy name parsing)
            let _ = std::fs::remove_dir_all(format!("/tmp/cistella-{session_id}"));
            println!("stopped {name}");
        }
        Command::Status { session_id } => {
            let filter = if let Some(id) = session_id {
                format!("label=cistella.session-id={id}")
            } else {
                "label=cistella.session-id".to_string()
            };
            let out = std::process::Command::new("podman")
                .args([
                    "ps",
                    "--all",
                    "--filter",
                    &filter,
                    "--format",
                    "{{.Names}} {{.Status}} {{.Label \"cistella.harness\"}}",
                ])
                .output()
                .map_err(|e| cistella::error::CistellaError::Runtime(format!("podman ps: {e}")))?;
            print!("{}", String::from_utf8_lossy(&out.stdout));
        }
        Command::Logs {
            session_id,
            harness,
        } => {
            let name = format!("cistella-{harness}-{session_id}");
            let logs = logs_container(&name)?;
            print!("{logs}");
        }
        Command::Gc => {
            let res = gc_exited()?;
            if res.reaped.is_empty() {
                println!("gc: nothing to reap");
            } else {
                for n in res.reaped {
                    println!("reaped {n}");
                }
            }
        }
        Command::Exec {
            session_id,
            harness,
            command,
        } => {
            let container = format!("cistella-{harness}-{session_id}");
            // Validate session fields charset to prevent injection in podman args (defense in depth)
            for (field, val) in [("session_id", &session_id), ("harness", &harness)] {
                if val.contains('\n') || val.contains('\r') || val.contains('\0') {
                    return Err(cistella::error::CistellaError::Runtime(format!(
                        "{field} must not contain control characters"
                    )));
                }
            }
            let cmd: Vec<String> = if command.is_empty() {
                vec!["/bin/sh".to_string()]
            } else {
                command
            };
            let args = cistella::transport::exec_args(&container, &cmd);
            let mut child = std::process::Command::new("podman")
                .args(&args)
                .stdin(std::process::Stdio::inherit())
                .stdout(std::process::Stdio::inherit())
                .stderr(std::process::Stdio::inherit())
                .spawn()
                .map_err(|e| {
                    cistella::error::CistellaError::Runtime(format!("podman exec: {e}"))
                })?;
            let status = child
                .wait()
                .map_err(|e| cistella::error::CistellaError::Runtime(format!("wait: {e}")))?;
            std::process::exit(status.code().unwrap_or(1));
        }
        Command::Doctor => {
            cistella::preflight::run_preflight()?;
            println!("doctor: ok");
        }
    }
    Ok(())
}

fn load_profile(
    path: Option<&str>,
    harness: &str,
) -> Result<Profile, cistella::error::CistellaError> {
    if let Some(p) = path {
        return Profile::from_file(std::path::Path::new(p));
    }
    // Validate harness before interpolating into TOML (prevent injection)
    if harness.is_empty()
        || harness.len() > 64
        || !harness
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
        || harness.starts_with('-')
        || harness.contains('\n')
        || harness.contains('\r')
    {
        return Err(cistella::error::CistellaError::Profile(format!(
            "harness must match [A-Za-z0-9._-]: {harness}"
        )));
    }
    // Default synthetic profile per harness spec (allowlist triples)
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/me".to_string());
    let text = format!(
        r#"
harness = "{harness}"
credential_surface = "none"
container_home = "/home/cistella"
mounts = [
  {{ host_source = "{home}/.config/opencode", container_target = "/home/cistella/.config/opencode", mode = "ro" }},
  {{ host_source = "{home}/.local/share/opencode", container_target = "/home/cistella/.local/share/opencode", mode = "rw" }},
  {{ host_source = "{home}/.local/state/opencode", container_target = "/home/cistella/.local/state/opencode", mode = "rw" }},
]
"#,
    );
    Profile::from_toml(&text)
}
