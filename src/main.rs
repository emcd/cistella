//! Cistella CLI entry point.

mod cli_conduct;
mod cli_util;

use crate::cli_conduct::conduct_session;
use crate::cli_util::{canonical_directory, describe_state, parse_label_filters, select_exact};
use cistella::cli::{Cli, Command, DescribeTarget};
use cistella::framework::contract::{Deadlines, ReconciliationKey};
use cistella::framework::isolator::Isolator;
use cistella::isolators::podman::PodmanIsolator;
use cistella::registry::{filter_records, list_sessions};
use cistella::runtime::{gc_exited, logs_container};
use clap::Parser;

fn main() -> std::process::ExitCode {
    // Standard CLI hygiene: Rust ignores SIGPIPE by default, which turns
    // `cistella inspect | head -1` into a panic instead of a quiet exit.
    unsafe {
        use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, sigaction};
        let action = SigAction::new(SigHandler::SigDfl, SaFlags::empty(), SigSet::empty());
        let _ = sigaction(Signal::SIGPIPE, &action);
    }
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
        Command::Conduct {
            profile,
            session_directory,
            identity,
            labels,
            image,
            configuration_directory,
            mounts,
            project_name,
            supplements,
            command,
        } => {
            // Exit-code frame: conduct returned, so its scope —
            // notably the staging guards — already dropped. The
            // single `process::exit` for conduct lives here.
            let code = conduct_session(
                &profile,
                session_directory,
                identity,
                &labels,
                image,
                configuration_directory,
                &mounts,
                project_name,
                &supplements,
                &command,
            )?;
            std::process::exit(code);
        }
        Command::Enter {
            id,
            directory,
            labels,
            command,
        } => {
            let record = select_exact(id.as_deref(), directory.as_deref(), &labels)?;
            // Companion exec is deliberately OUTSIDE the Landlock
            // guarantee (operator decision): the operator is
            // trusted and already holds host-equivalent access, so
            // restricting their own shell would be overengineering
            // against no defended principal. Only the wrapped
            // harness lineage is confined; a sidecar sees the
            // Podman binds as mounted. The hooked marker on the
            // record stays as confinement state (and as the signal
            // a future wrapper-sidecar path will key on).
            let container = record.container_name;
            for (field, val) in [("session", &container)] {
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
            exit_with_status(status);
        }
        Command::Survey { directory, labels } => {
            let records = list_sessions()?;
            let parsed = parse_label_filters(&labels)?;
            let canon_dir = directory.as_deref().map(canonical_directory).transpose()?;
            let shown = filter_records(&records, canon_dir.as_deref(), &parsed);
            for r in shown {
                println!(
                    "{} {} profile={} directory={} identity={}",
                    r.container_name, r.active_state, r.profile, r.directory, r.identity
                );
            }
            Ok(())
        }
        Command::Inspect {
            id,
            directory,
            labels,
        } => {
            let record = select_exact(id.as_deref(), directory.as_deref(), &labels)?;
            println!(
                "{} profile={} directory={} identity={} image={}",
                record.container_name,
                record.profile,
                record.directory,
                record.identity,
                record.image
            );
            let logs = logs_container(&record.container_name)?;
            print!("{logs}");
            Ok(())
        }
        Command::Terminate {
            id,
            directory,
            labels,
        } => {
            // Scan-and-teardown holds the creation-window lock throughout:
            // resolving first and locking only for teardown could observe a
            // half-installed unit and reap a session being born.
            let _guard = cistella::lock::LockGuard::acquire()?;
            let record = select_exact(id.as_deref(), directory.as_deref(), &labels)?;
            let isolator = PodmanIsolator::new();
            let handle = isolator.adopt(&record.container_name, &record.id);
            let key = ReconciliationKey::generate();
            let grace = Deadlines::default().terminate_grace;
            isolator.terminate(&handle, grace, &key)?;
            isolator.remove(&handle, &key)?;
            cistella::cleanup::reap_and_report(&cistella::lock::runtime_base(), &record.id);
            cistella::attestation::reap(&cistella::lock::runtime_base(), &record.id);
            println!("terminate {}", record.container_name);
            Ok(())
        }
        Command::Describe { target } => match target {
            DescribeTarget::State {
                id,
                directory,
                labels,
            } => describe_state(id.as_deref(), directory.as_deref(), &labels),
        },
        Command::Gc => {
            let res = gc_exited()?;
            if res.reaped.is_empty() {
                println!("gc: nothing to reap");
            } else {
                for n in res.reaped {
                    println!("reaped {n}");
                }
            }
            Ok(())
        }
        Command::Check => {
            cistella::preflight::run_preflight()?;
            println!("check: ok");
            Ok(())
        }
    }
}
/// Exits the process with a waited child's disposition (or 128+signal).
fn exit_with_status(status: std::process::ExitStatus) -> ! {
    use std::os::unix::process::ExitStatusExt;
    if let Some(signal) = status.signal() {
        std::process::exit(128 + signal);
    }
    std::process::exit(status.code().unwrap_or(1));
}
