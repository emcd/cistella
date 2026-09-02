//! CLI definitions for the cistella driver.

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "cistella", version, about = "Driver for agent dev containers")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Create and start a session container.
    Run {
        /// Session id.
        #[arg(long)]
        session_id: String,
        /// Seat name.
        #[arg(long)]
        seat: String,
        /// Harness name.
        #[arg(long)]
        harness: String,
        /// Profile name.
        #[arg(long)]
        profile: String,
        /// Worktree host path (required; becomes container /work).
        #[arg(long)]
        worktree: String,
        /// Profile file path.
        #[arg(long)]
        profile_file: Option<String>,
        /// Image ref.
        #[arg(long, default_value = "cistella/opencode:example")]
        image: String,
    },
    /// Stop a session container.
    Stop {
        #[arg(long)]
        session_id: String,
        #[arg(long)]
        harness: String,
    },
    /// Status of session containers.
    Status {
        #[arg(long)]
        session_id: Option<String>,
    },
    /// Logs of a session container.
    Logs {
        #[arg(long)]
        session_id: String,
        #[arg(long)]
        harness: String,
    },
    /// Reap exited cistella containers.
    Gc,
    /// Execute a command inside the session container with closed env forwarded.
    Exec {
        /// Session id.
        #[arg(long)]
        session_id: String,
        /// Harness name.
        #[arg(long)]
        harness: String,
        /// Command to execute (default: shell).
        #[arg(last = true)]
        command: Vec<String>,
    },
    /// Host preflight / doctor.
    Doctor,
}
