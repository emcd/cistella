//! CLI definitions for the cistella driver.
//!
//! All-Latinate verb slate: `conduct` owns the session lifetime,
//! `enter` provides companion entry, `survey` lists, `inspect` shows
//! post-mortem, `terminate` tears down from outside, `gc` reaps orphans,
//! `check` runs host preflight. No aliases on verbs; `--cwd` is the sole
//! flag alias (hidden) for `--session-directory`.

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "cistella", version, about = "Driver for agent dev containers")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Mint a session id, create and start the container, exec the
    /// harness argv on the pane PTY, wait, then tear down.
    Conduct {
        /// Profile name (supplied configuration directory, then XDG with
        /// seed-if-absent, then baked examples) or file path.
        #[arg(long)]
        profile: String,
        /// Configuration directory naming `<dir>/profiles/<name>.toml`
        /// as a closed tier (flag beats `$CISTELLA_CONFIGURATION_DIRECTORY`;
        /// conduct only).
        #[arg(long)]
        configuration_directory: Option<String>,
        /// Session worktree `<host>[:<container>]` (host directory
        /// mounted at the container target, default `/work`; optional,
        /// host defaults to cwd).
        #[arg(long, alias = "cwd")]
        session_directory: Option<String>,
        /// Extra mount triple `<host>:<target>:<mode>` (repeatable; exact
        /// profile-target matches override, partial overlaps fail).
        #[arg(long = "mount")]
        mounts: Vec<String>,
        /// Project name for `{{project-name}}` templates (defaults to the
        /// basename of the canonical session directory).
        #[arg(long)]
        project_name: Option<String>,
        /// Identity label (not a credential selector).
        #[arg(long)]
        identity: Option<String>,
        /// Generic label `k=v` for orchestrator correlation (repeatable;
        /// `cistella.` prefix refused).
        #[arg(long = "label")]
        labels: Vec<String>,
        /// Image ref override (profile `image` otherwise).
        #[arg(long)]
        image: Option<String>,
        /// Harness argv after `--` (profile `command` array otherwise).
        #[arg(last = true)]
        command: Vec<String>,
    },
    /// Enter a session with a companion shell via `podman exec -i -t`.
    Enter {
        /// Session id unique prefix (exactly one selector per invocation).
        id: Option<String>,
        /// Select by canonical host directory.
        #[arg(long)]
        directory: Option<String>,
        /// Select by generic label `k=v` (repeatable, ANDed).
        #[arg(long = "label")]
        labels: Vec<String>,
        /// Command to execute (default: shell).
        #[arg(last = true)]
        command: Vec<String>,
    },
    /// Survey sessions by joining the unit registry with runtime state.
    Survey {
        /// Filter by canonical host directory.
        #[arg(long)]
        directory: Option<String>,
        /// Filter by generic label `k=v` (repeatable, ANDed).
        #[arg(long = "label")]
        labels: Vec<String>,
    },
    /// Inspect a session (labels plus journald post-mortem).
    Inspect {
        /// Session id unique prefix (exactly one selector per invocation).
        id: Option<String>,
        /// Select by canonical host directory.
        #[arg(long)]
        directory: Option<String>,
        /// Select by generic label `k=v` (repeatable, ANDed).
        #[arg(long = "label")]
        labels: Vec<String>,
    },
    /// Terminate a session from outside via shared teardown.
    Terminate {
        /// Session id unique prefix (exactly one selector per invocation).
        id: Option<String>,
        /// Select by canonical host directory.
        #[arg(long)]
        directory: Option<String>,
        /// Select by generic label `k=v` (repeatable, ANDed).
        #[arg(long = "label")]
        labels: Vec<String>,
    },
    /// Reap orphaned cistella units and scratch.
    Gc,
    /// Host preflight check.
    Check,
}
