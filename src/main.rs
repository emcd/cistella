//! Cistella CLI entry point.
//!
//! External driver invoked by Agentmux coder profiles. PoC scope and
//! design-session verdicts are recorded at `home:coordination/6` and
//! the durable architecture note at `home:ideas/projects/8`. This
//! binary is a placeholder while the driver implementation is
//! drafted.
use cistella::version;

fn main() -> std::process::ExitCode {
    println!("cistella {} (scaffold only)", version());
    std::process::ExitCode::SUCCESS
}
