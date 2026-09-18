//! Mountpoint-preparation helpers: canonical bind set and stat parsing.

use std::collections::HashMap;

use cistella::mount::volume_targets;
use cistella::prepare::parse_stat_map;

#[test]
fn volume_targets_parse_and_canonicalize() {
    // Single source of truth: every emitted --volume entry contributes
    // its container side in canonical form, so authorization cannot miss
    // a mount prefix whose profile spelling was noncanonical. Tmpfs
    // entries contribute nothing (home is handled separately).
    let volumes = vec![
        "--tmpfs".to_string(),
        "/home/cistella".to_string(),
        "--volume".to_string(),
        "/data/./x:/home/cistella/.config//deep:rw".to_string(),
        "--volume".to_string(),
        "/run/seat.sock:/run/cistella/seat.sock:ro".to_string(),
    ];
    assert_eq!(
        volume_targets(&volumes),
        vec![
            "/home/cistella/.config/deep".to_string(),
            "/run/cistella/seat.sock".to_string(),
        ]
    );
}

#[test]
fn parse_stat_map_survives_spaces() {
    // `%u %n` puts the uid first so names with spaces parse; missing
    // paths have no entry (the created-vs-preexisting signal).
    let map = parse_stat_map("0 /home/cistella/.config\n1000 /home/cistella/my dir\n");
    assert_eq!(
        map.get("/home/cistella/.config").map(String::as_str),
        Some("0")
    );
    assert_eq!(
        map.get("/home/cistella/my dir").map(String::as_str),
        Some("1000")
    );
    assert_eq!(map.len(), 2);
}

#[test]
fn parse_stat_map_empty() {
    let map: HashMap<String, String> = parse_stat_map("");
    assert!(map.is_empty());
}

use cistella::mount::{MountMode, MountTriple, preparation_sources};

fn triple(host: &str, target: &str, mode: MountMode) -> MountTriple {
    MountTriple {
        host_source: host.to_string(),
        container_target: target.to_string(),
        mode,
    }
}

#[test]
fn preparation_sources_cover_user_mounts_only() {
    // Profile + CLI + session targets seed candidates; scratch and
    // credential-style internals never do (they join the authorization
    // set via volume_targets, not here).
    let profile = vec![triple("/data", "/home/cistella/.config/x", MountMode::Rw)];
    let cli = vec![triple("/n", "/srv/data", MountMode::Ro)];
    let got = preparation_sources(&profile, &cli, "/work");
    assert!(
        got.contains(&"/home/cistella/.config/x".to_string()),
        "{got:?}"
    );
    assert!(got.contains(&"/srv/data".to_string()), "{got:?}");
    assert!(got.contains(&"/work".to_string()), "{got:?}");
}
