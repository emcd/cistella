//! `tmp_backing_probe` — live bind-alias refusal probe for the
//! baseline `/tmp` grant.
//!
//! The table-relative alias class (`mount_alias`) is fast-pinned
//! with fixture tables, but a REAL bind mount is the only proof
//! the refusal fires on adversarial dentries: `canonicalize`
//! resolves symlinks, never binds, so only a same-dentry-
//! different-path mount exercises the containment leg. This
//! helper builds the exact triple shape conduct refuses
//! (baseline-backed `/tmp` plus one RO alias) and runs the
//! production sets derivation (`hook_full_sets`) plus the
//! baseline preflight, printing `ALLOWED` or `REFUSED:<message>`
//! and exiting 0 either way — the verdict rides stdout, never
//! the status. Callers create the bind inside a private mount
//! namespace (`podman unshare` + `mount --bind`:
//! unprivileged-capable, evaporates with the namespace, zero
//! host mutation) and skip quietly where unavailable.
//!
//! Declared `[[example]]` (lands at `target/<profile>/examples/`);
//! out of `package.include`. Test-only, never shipped.

use cistella::framework::hooks::hook_full_sets;
use cistella::mount::{MountMode, MountTriple};
use cistella::mount_alias::baseline_tmp_alias_preflight;

fn usage() -> ! {
    eprintln!(
        "usage: tmp_backing_probe <home> <session-dir> <backing> <alias-source> <alias-target>"
    );
    std::process::exit(2);
}

fn main() {
    let mut argv = std::env::args();
    let _program = argv.next();
    let (Some(home), Some(session), Some(backing), Some(alias_source), Some(alias_target)) = (
        argv.next(),
        argv.next(),
        argv.next(),
        argv.next(),
        argv.next(),
    ) else {
        usage();
    };
    // The sets derivation reads confinement roots from HOME.
    unsafe {
        std::env::set_var("HOME", &home);
    }
    let ancestor = format!("{home}/src");
    let hook = cistella::framework::contract::GuestHookRequest {
        artifact: cistella::framework::contract::HookArtifact {
            kind: "digest-pinned-blob".to_string(),
            sha256: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_string(),
            source: cistella::framework::contract::HookSource {
                registry: "shipped".to_string(),
                path: "cistella-supervisor-landlock".to_string(),
            },
        },
        staging: "isolator-staged".to_string(),
        order: 0,
        argv_prefix: vec!["/run/cistella/hooks/supervisor-landlock".to_string()],
        probe: cistella::framework::contract::HookProbe {
            op: "probe".to_string(),
            timeout_ms: 1000,
        },
        on_failure: "fail-pre-exec".to_string(),
    };
    let triples = vec![
        MountTriple {
            host_source: ancestor,
            container_target: "/src".to_string(),
            mode: MountMode::Ro,
        },
        MountTriple {
            host_source: backing,
            container_target: "/tmp".to_string(),
            mode: MountMode::Ro,
        },
        MountTriple {
            host_source: alias_source,
            container_target: alias_target,
            mode: MountMode::Ro,
        },
    ];
    let placements: Vec<cistella::mount::Placement> = triples
        .iter()
        .map(|triple| cistella::mount::Placement {
            host_source: triple.host_source.clone(),
            container_target: triple.container_target.clone(),
        })
        .collect();
    let effective =
        cistella::framework::contract::EffectivePermissions::assemble(&triples, &[], &[], &[])
            .expect("driver-only assembly");
    let sets = match hook_full_sets(
        std::slice::from_ref(&hook),
        &placements,
        &effective,
        &session,
    ) {
        Ok(sets) => sets,
        Err(error) => {
            println!("REFUSED:{error}");
            return;
        }
    };
    match baseline_tmp_alias_preflight(&triples, &effective, &sets.routes, &sets.sources) {
        Ok(()) => println!("ALLOWED"),
        Err(error) => println!("REFUSED:{error}"),
    }
}
