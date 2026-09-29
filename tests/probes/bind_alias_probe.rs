//! `bind_alias_probe` — live bind-alias refusal probe for 3.4 part 2.
//!
//! The dev+ino hardening (`mount_alias`) is fast-pinned with
//! fixture tables and scripted ids, but a REAL bind mount is
//! the only proof the refusal fires on adversarial dentries:
//! `canonicalize` resolves symlinks, never binds, so only a
//! same-dentry-different-path mount exercises the identity
//! leg. This helper builds the exact triple shape conduct
//! refuses (ancestor RO plus one read-write graft) and runs
//! the production preflight entry (`hook_graft_alias_preflight`),
//! printing `ALLOWED` or `REFUSED:<message>` and exiting 0
//! either way — the verdict rides stdout, never the status.
//! Callers create the bind inside a private mount namespace
//! (`podman unshare` + `mount --bind`: unprivileged-capable,
//! evaporates with the namespace, zero host mutation) and
//! skip quietly where unavailable.
//!
//! Declared `[[example]]` (lands at `target/<profile>/examples/`);
//! out of `package.include`. Test-only, never shipped.

use cistella::framework::hooks::hook_graft_alias_preflight;
use cistella::mount::{MountMode, MountTriple};

fn usage() -> ! {
    eprintln!("usage: bind_alias_probe <home> <session-dir> <graft-source> <graft-target>");
    std::process::exit(2);
}

fn main() {
    let mut argv = std::env::args();
    let _program = argv.next();
    let (Some(home), Some(session), Some(source), Some(target)) =
        (argv.next(), argv.next(), argv.next(), argv.next())
    else {
        usage();
    };
    // The preflight reads confinement roots from HOME.
    unsafe {
        std::env::set_var("HOME", &home);
    }
    let ancestor = format!("{home}/src");
    let triples = vec![
        MountTriple {
            host_source: ancestor,
            container_target: "/src".to_string(),
            mode: MountMode::Ro,
        },
        MountTriple {
            host_source: source,
            container_target: target,
            mode: MountMode::Rw,
        },
    ];
    match hook_graft_alias_preflight(&triples, &session) {
        Ok(()) => println!("ALLOWED"),
        Err(error) => println!("REFUSED:{error}"),
    }
}
