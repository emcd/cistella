//! Live baseline-alias refusal: bind-hidden containment for the
//! unconditional `/tmp` grant (tmpdir-simplification HIGH).
//!
//! Sibling file to `mountprep.rs` (which outgrew the line gate):
//! same `podman unshare` capability pattern, same quiet skip.

use std::process::Command;

use tempfile::TempDir;

use super::helpers::*;

/// Live baseline-alias refusal (tmpdir-simplification HIGH):
/// a REAL bind mount shares dentries across spellings that
/// canonicalize apart, so only the table-relative containment
/// leg can see it. Three cases run inside one private mount
/// namespace (`podman unshare` + `mount --bind`:
/// unprivileged-capable, evaporates with the namespace — zero
/// host mutation), plus an undisjointed control that must stay
/// allowed. Skips quietly where the capability is absent.
#[ignore = "live: requires bind-mount capability (podman unshare)"]
#[test]
fn bind_alias_tmp_backing_refuses_precreate() {
    let unshare = Command::new("podman")
        .arg("unshare")
        .arg("true")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false);
    if !unshare {
        eprintln!("skip: podman unshare unavailable");
        return;
    }
    // Fake HOME confinement tree: ancestor $HOME/src with a
    // project subtree and a sibling; the sibling is bind-exposed
    // at a disjoint path (the baseline backing).
    let home = TempDir::new().expect("fake home");
    let src = home.path().join("src");
    let proj = src.join("proj");
    let sib = src.join("sib");
    std::fs::create_dir_all(&proj).expect("proj dir");
    std::fs::create_dir_all(&sib).expect("sib dir");
    std::fs::write(sib.join("seed"), "seed").expect("sib seed");
    // Disjoint mountpoints, a disjoint control backing, and an
    // outside tree (outside the ancestor, for the exact case so
    // the ancestor cannot fire first — declaration order would
    // otherwise report the covering tree instead of the alias).
    let alias = TempDir::new().expect("alias mountpoint");
    let control = TempDir::new().expect("control backing");
    let outside = TempDir::new().expect("outside source tree");
    let boutside = TempDir::new().expect("outside bind mountpoint");
    let boutside2 = TempDir::new().expect("outside bind-of-bind mountpoint");
    std::fs::write(outside.path().join("seed"), "seed").expect("outside seed");
    let probe = example_binary("tmp_backing_probe");
    let home_str = home.path().to_string_lossy().to_string();
    let proj_str = proj.to_string_lossy().to_string();
    let src_str = src.to_string_lossy().to_string();
    let sib_str = sib.to_string_lossy().to_string();
    let alias_str = alias.path().to_string_lossy().to_string();
    let control_str = control.path().to_string_lossy().to_string();
    let outside_str = outside.path().to_string_lossy().to_string();
    let boutside_str = boutside.path().to_string_lossy().to_string();
    let boutside2_str = boutside2.path().to_string_lossy().to_string();
    let probe_str = probe.to_string_lossy().to_string();
    // All cases in one namespace: subdir bind (sibling at a
    // disjoint path backing /tmp, ancestor covers it — table
    // containment, not spelling), exact bind (outside tree
    // exposed at two disjoint bind spellings — same dentries,
    // no ancestor in play), and undisjointed control (must
    // stay allowed). `;`-chained so a mount failure surfaces
    // loudly, never as a silent pass. Identity lines prove the
    // binds are real and that the subdir case is NOT an
    // exact-ID match.
    let script = format!(
        "mount --bind '{sib_str}' '{alias_str}' || echo MOUNT_FAILED_SIB; \
         mount --bind '{outside_str}' '{boutside_str}' || echo MOUNT_FAILED_OUTSIDE; \
         mount --bind '{boutside_str}' '{boutside2_str}' || echo MOUNT_FAILED_OUTSIDE2; \
         echo -n 'ids-src:'; stat -c '%d %i' '{src_str}'; \
         echo -n 'ids-sib:'; stat -c '%d %i' '{sib_str}'; \
         echo -n 'ids-alias:'; stat -c '%d %i' '{alias_str}'; \
         echo -n 'ids-outside:'; stat -c '%d %i' '{outside_str}'; \
         echo -n 'ids-boutside2:'; stat -c '%d %i' '{boutside2_str}'; \
         echo -n 'case-subdir:'; '{probe_str}' '{home_str}' '{proj_str}' '{alias_str}' '{control_str}' /mirror; \
         echo -n 'case-exact:'; '{probe_str}' '{home_str}' '{proj_str}' '{boutside_str}' '{boutside2_str}' /mirror; \
         echo -n 'case-control:'; '{probe_str}' '{home_str}' '{proj_str}' '{control_str}' '{control_str}' /mirror"
    );
    let out = Command::new("podman")
        .arg("unshare")
        .arg("env")
        .arg(format!("HOME={home_str}"))
        .arg("sh")
        .arg("-c")
        .arg(&script)
        .output()
        .expect("podman unshare must spawn");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(out.status.success(), "unshare script must run: {stderr}");
    assert!(
        !stdout.contains("MOUNT_FAILED"),
        "bind mounts must establish: {stdout} {stderr}"
    );
    let ids = |label: &str| {
        stdout
            .lines()
            .find(|line| line.starts_with(label))
            .unwrap_or_else(|| panic!("missing {label} in:\n{stdout}"))
            .trim_start_matches(label)
            .to_string()
    };
    let src_ids = ids("ids-src:");
    let sib_ids = ids("ids-sib:");
    let alias_ids = ids("ids-alias:");
    let outside_ids = ids("ids-outside:");
    let boutside2_ids = ids("ids-boutside2:");
    assert_eq!(
        alias_ids, sib_ids,
        "alias bind must share the sibling dentry, got alias={alias_ids} sib={sib_ids}"
    );
    assert_ne!(
        sib_ids, src_ids,
        "subdir case must differ from the ancestor dentry (else exact-ID equality decides, not containment)"
    );
    assert_eq!(
        boutside2_ids, outside_ids,
        "bind-of-bind must share the outside dentry, got b2={boutside2_ids} outside={outside_ids}"
    );
    let line = |label: &str| {
        stdout
            .lines()
            .find(|line| line.starts_with(label))
            .unwrap_or_else(|| panic!("missing {label} in:\n{stdout}"))
            .to_string()
    };
    // Subdir bind backing /tmp with the ancestor covering it:
    // the ancestor would flip to RW — refuse naming /src.
    let subdir = line("case-subdir:");
    assert!(
        subdir.starts_with("case-subdir:REFUSED:") && subdir.contains("/src"),
        "subdir bind alias must refuse naming the ancestor, got: {subdir}"
    );
    // Exact bind (bind-of-bind outside the ancestor) as RO alias
    // at /mirror: same dentries, disjoint spelling, no ancestor
    // in play — refuse naming /mirror.
    let exact = line("case-exact:");
    assert!(
        exact.starts_with("case-exact:REFUSED:") && exact.contains("/mirror"),
        "exact bind alias must refuse naming the alias, got: {exact}"
    );
    // Control: disjoint backing stays allowed.
    assert_eq!(
        line("case-control:"),
        "case-control:ALLOWED",
        "disjoint backing must stay allowed"
    );
    eprintln!("tmp-backing bind cases EXECUTED: subdir+exact refused, control allowed");
}
