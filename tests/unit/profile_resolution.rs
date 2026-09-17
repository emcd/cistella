//! Profile resolution: tiered lookup, closed supplied tiers, seed-if-absent.
//!
//! All fixtures use tempdirs passed explicitly as lookup inputs, so no test
//! touches the real cwd, `XDG_CONFIG_HOME`, or `HOME` — except
//! `env_tier_end_to_end`, the single test that mutates process env, which
//! saves and restores every var it sets.

use std::path::{Path, PathBuf};

use tempfile::TempDir;

use cistella::profile::{Profile, ProjectName, ResolutionSource};

fn minimal_toml(image: &str) -> String {
    format!("image = \"{image}\"\ncredential_surface = \"none\"\nmounts = []\n")
}

fn write_profile(dir: &Path, name: &str, image: &str) -> PathBuf {
    let path = dir.join(format!("{name}.toml"));
    std::fs::write(&path, minimal_toml(image)).expect("write profile fixture");
    path
}

/// Lookup inputs with no supplied tiers and a fresh (nonexistent) XDG dir.
fn fresh_source(xdg_base: &TempDir) -> (ResolutionSource, PathBuf) {
    let xdg = xdg_base.path().join("xdg-profiles");
    (ResolutionSource::new(Vec::new(), xdg.clone()), xdg)
}

#[test]
fn baked_name_resolves_and_seeds() {
    let xdg_base = TempDir::new().unwrap();
    let (source, xdg) = fresh_source(&xdg_base);
    let (prof, digest, name) = Profile::resolve_in("default", &source, None).unwrap();
    assert_eq!(name, "default");
    // Self-consistent with the seeded copy rather than hardcoded example
    // strings, so editing `data/profiles/default.toml` does not break this
    // without breaking behavior; only the baked name itself is pinned.
    let seeded = std::fs::read_to_string(xdg.join("default.toml")).unwrap();
    assert_eq!(digest, Profile::digest_of(&seeded));
    let seeded_profile = Profile::from_toml(&seeded).unwrap();
    assert_eq!(prof.image, seeded_profile.image);
    assert!(
        xdg.join("opencode.toml").exists(),
        "all baked examples seed"
    );
}

#[test]
fn xdg_copy_wins_over_baked_and_survives_seed() {
    let xdg_base = TempDir::new().unwrap();
    let (source, xdg) = fresh_source(&xdg_base);
    std::fs::create_dir_all(&xdg).unwrap();
    let custom = "localhost/custom:mine";
    write_profile(&xdg, "opencode", custom);
    let before = std::fs::read(xdg.join("opencode.toml")).unwrap();
    let (prof, _digest, name) = Profile::resolve_in("opencode", &source, None).unwrap();
    assert_eq!(name, "opencode");
    assert_eq!(prof.image, custom);
    assert_eq!(
        std::fs::read(xdg.join("opencode.toml")).unwrap(),
        before,
        "seed never overwrites user files"
    );
    assert!(
        xdg.join("default.toml").exists(),
        "missing baked still seeds"
    );
}

#[test]
fn flag_dir_wins_over_env_dir_and_xdg() {
    let flag_base = TempDir::new().unwrap();
    let env_base = TempDir::new().unwrap();
    let xdg_base = TempDir::new().unwrap();
    let (source, xdg) = fresh_source(&xdg_base);
    let flag_profiles = flag_base.path().join("profiles");
    let env_profiles = env_base.path().join("profiles");
    std::fs::create_dir_all(&flag_profiles).unwrap();
    std::fs::create_dir_all(&env_profiles).unwrap();
    std::fs::create_dir_all(&xdg).unwrap();
    write_profile(&flag_profiles, "custom", "localhost/flag:1");
    write_profile(&env_profiles, "custom", "localhost/env:1");
    write_profile(&xdg, "custom", "localhost/xdg:1");
    // Flag first in the precedence list wins.
    let source = ResolutionSource::new(
        vec![
            flag_base.path().to_path_buf(),
            env_base.path().to_path_buf(),
        ],
        source.xdg_profiles_dir,
    );
    let (prof, _, _) = Profile::resolve_in("custom", &source, None).unwrap();
    assert_eq!(prof.image, "localhost/flag:1");
    // With only the env dir supplied, it wins over XDG.
    let source = ResolutionSource::new(
        vec![env_base.path().to_path_buf()],
        xdg_base.path().join("xdg-profiles"),
    );
    std::fs::create_dir_all(xdg_base.path().join("xdg-profiles")).unwrap();
    write_profile(
        &xdg_base.path().join("xdg-profiles"),
        "custom",
        "localhost/xdg:1",
    );
    let (prof, _, _) = Profile::resolve_in("custom", &source, None).unwrap();
    assert_eq!(prof.image, "localhost/env:1");
}

#[test]
fn flag_dir_miss_shadows_env_and_writes_nothing() {
    let flag_base = TempDir::new().unwrap();
    let env_base = TempDir::new().unwrap();
    let xdg_base = TempDir::new().unwrap();
    let xdg = xdg_base.path().join("xdg-profiles");
    std::fs::create_dir_all(flag_base.path().join("profiles")).unwrap();
    std::fs::create_dir_all(env_base.path().join("profiles")).unwrap();
    write_profile(
        &env_base.path().join("profiles"),
        "custom",
        "localhost/env:1",
    );
    // Setup creates the env profiles dir above; flag dir exists but lacks
    // the name, XDG holds a copy, and baked holds `default` — all must lose
    // to the closed flag tier.
    let source = ResolutionSource::new(
        vec![
            flag_base.path().to_path_buf(),
            env_base.path().to_path_buf(),
        ],
        xdg.clone(),
    );
    let err = Profile::resolve_in("custom", &source, None).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("custom"), "error names the profile: {msg}");
    assert!(!xdg.exists(), "closed miss writes nothing to XDG");
}

#[test]
fn supplied_closed_miss_never_falls_through_to_baked() {
    let supplied_base = TempDir::new().unwrap();
    let xdg_base = TempDir::new().unwrap();
    let xdg = xdg_base.path().join("xdg-profiles");
    std::fs::create_dir_all(supplied_base.path().join("profiles")).unwrap();
    let source = ResolutionSource::new(vec![supplied_base.path().to_path_buf()], xdg.clone());
    // `default` is baked, but the supplied tier is closed.
    let err = Profile::resolve_in("default", &source, None).unwrap_err();
    assert!(err.to_string().contains("default"));
    assert!(!xdg.exists(), "closed tier never seeds");
}

#[test]
fn unknown_name_errors_and_still_seeds_baked() {
    let xdg_base = TempDir::new().unwrap();
    let (source, xdg) = fresh_source(&xdg_base);
    let err = Profile::resolve_in("nope", &source, None).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("nope"), "error names the profile: {msg}");
    assert!(msg.contains("baked"), "error lists tiers searched: {msg}");
    assert!(
        xdg.join("default.toml").exists(),
        "reaching the default tier seeds even on miss"
    );
}

#[test]
fn explicit_path_bypasses_every_tier() {
    let work = TempDir::new().unwrap();
    let supplied_base = TempDir::new().unwrap();
    let xdg_base = TempDir::new().unwrap();
    let xdg = xdg_base.path().join("xdg-profiles");
    // Decoys in every named tier must not shadow the explicit file.
    std::fs::create_dir_all(supplied_base.path().join("profiles")).unwrap();
    write_profile(
        &supplied_base.path().join("profiles"),
        "custom",
        "localhost/decoy:1",
    );
    let path = write_profile(work.path(), "custom", "localhost/explicit:1");
    let source = ResolutionSource::new(vec![supplied_base.path().to_path_buf()], xdg.clone());
    let (prof, _digest, name) =
        Profile::resolve_in(&path.to_string_lossy(), &source, None).unwrap();
    assert_eq!(prof.image, "localhost/explicit:1");
    assert_eq!(name, "custom", "registry name is the file stem");
    assert!(!xdg.exists(), "explicit paths never seed");
}

#[test]
fn explicit_missing_path_errors_without_scaffolding() {
    let work = TempDir::new().unwrap();
    let xdg_base = TempDir::new().unwrap();
    let xdg = xdg_base.path().join("xdg-profiles");
    let source = ResolutionSource::new(Vec::new(), xdg.clone());
    let missing = work.path().join("gone.toml");
    let err = Profile::resolve_in(&missing.to_string_lossy(), &source, None).unwrap_err();
    assert!(err.to_string().contains("gone.toml"));
    assert!(!xdg.exists(), "explicit miss creates nothing");
}

#[test]
fn env_tier_end_to_end() {
    // Sole test that mutates process env; saves and restores everything.
    let env_base = TempDir::new().unwrap();
    let xdg_base = TempDir::new().unwrap();
    let xdg = xdg_base.path().join("cistella/profiles");
    let env_profiles = env_base.path().join("profiles");
    std::fs::create_dir_all(&env_profiles).unwrap();
    write_profile(&env_profiles, "custom", "localhost/env:1");
    let old_config_dir = std::env::var("CISTELLA_CONFIGURATION_DIRECTORY").ok();
    let old_xdg = std::env::var("XDG_CONFIG_HOME").ok();
    unsafe {
        std::env::set_var("CISTELLA_CONFIGURATION_DIRECTORY", env_base.path());
        std::env::set_var("XDG_CONFIG_HOME", xdg_base.path());
    }
    let result = (|| {
        let source = ResolutionSource::from_host_env(None)?;
        assert_eq!(
            source.xdg_profiles_dir, xdg,
            "XDG_CONFIG_HOME honored for the default tier"
        );
        Profile::resolve_in("custom", &source, None)
    })();
    unsafe {
        match old_config_dir {
            Some(v) => std::env::set_var("CISTELLA_CONFIGURATION_DIRECTORY", v),
            None => std::env::remove_var("CISTELLA_CONFIGURATION_DIRECTORY"),
        }
        match old_xdg {
            Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
    }
    let (prof, _, name) = result.unwrap();
    assert_eq!(prof.image, "localhost/env:1");
    assert_eq!(name, "custom");
}

#[test]
fn from_host_env_puts_flag_first() {
    let flag = TempDir::new().unwrap();
    let source = ResolutionSource::from_host_env(Some(flag.path().to_str().unwrap())).unwrap();
    assert_eq!(
        source
            .configuration_directories
            .first()
            .map(|p| p.as_path()),
        Some(flag.path()),
        "flag leads the precedence list"
    );
}

fn template_toml(host: &str, target: &str) -> String {
    format!(
        "image = \"localhost/cistella/opencode:example\"\n\
         credential_surface = \"none\"\n\
         container_home = \"/home/cistella\"\n\
         command = [\"run\", \"{{{{project-name}}}}\"]\n\
         [[mounts]]\n\
         host_source = \"{host}\"\n\
         container_target = \"{target}\"\n\
         mode = \"rw\"\n"
    )
}

fn resolve_template_text(
    toml: &str,
    project: Option<ProjectName<'_>>,
) -> Result<(cistella::profile::Profile, String, String), cistella::error::CistellaError> {
    let work = TempDir::new().unwrap();
    let path = work.path().join("tmpl.toml");
    std::fs::write(&path, toml).unwrap();
    let xdg_base = TempDir::new().unwrap();
    let source = ResolutionSource::new(Vec::new(), xdg_base.path().join("xdg"));
    // Synchronous resolution completes before the TempDirs drop.
    Profile::resolve_in(&path.to_string_lossy(), &source, project)
}

#[test]
fn default_project_name_locks_basename() {
    use cistella::profile::default_project_name;
    // QA worktree-clone lock: basename, not notebook key.
    assert_eq!(
        default_project_name("/home/me/src/CLONES/cistella/qa").unwrap(),
        "qa"
    );
    assert_eq!(default_project_name("/repo").unwrap(), "repo");
    assert!(default_project_name("/").is_err());
}

#[test]
fn templates_expand_on_both_sides_and_argv() {
    let toml = template_toml(
        "/data/{{project-name}}",
        "{{container-home}}/{{project-name}}",
    );
    let (prof, _, _) = resolve_template_text(&toml, Some(ProjectName::Explicit("qa"))).unwrap();
    assert_eq!(prof.mounts[0].host_source, "/data/qa");
    assert_eq!(prof.mounts[0].container_target, "/home/cistella/qa");
    assert_eq!(
        prof.command.unwrap(),
        vec!["run".to_string(), "qa".to_string()]
    );
}

#[test]
fn project_override_beats_basename_default() {
    // QA-clone shape: directory says `qa`, flag says `cistella`.
    let toml = template_toml("/notes/{{project-name}}", "/notes/{{project-name}}");
    let (prof, _, _) =
        resolve_template_text(&toml, Some(ProjectName::Explicit("cistella"))).unwrap();
    assert_eq!(prof.mounts[0].host_source, "/notes/cistella");
}

#[test]
fn host_home_template_reads_home() {
    // Only mutates HOME-adjacent reads through the standard env; no other
    // test reads these template paths, and HOME itself is untouched.
    let toml = template_toml("{{host-home}}/.config/x", "/x");
    let (prof, _, _) = resolve_template_text(&toml, Some(ProjectName::Explicit("p"))).unwrap();
    let home = std::env::var("HOME").unwrap();
    assert_eq!(prof.mounts[0].host_source, format!("{home}/.config/x"));
}

#[test]
fn unknown_template_fails_closed() {
    let toml = template_toml("/data/{{nosuch}}", "/x");
    let err = resolve_template_text(&toml, Some(ProjectName::Explicit("p"))).unwrap_err();
    assert!(err.to_string().contains("nosuch"));
}

#[test]
fn template_without_context_fails_closed() {
    let toml = template_toml("/data/{{project-name}}", "/x");
    let err = resolve_template_text(&toml, None).unwrap_err();
    assert!(err.to_string().contains("project context"));
    // Literal profiles still resolve without context.
    let plain = minimal_toml("localhost/cistella/opencode:example");
    let work = TempDir::new().unwrap();
    let path = work.path().join("plain.toml");
    std::fs::write(&path, plain).unwrap();
    let xdg = TempDir::new().unwrap();
    let source = ResolutionSource::new(Vec::new(), xdg.path().join("xdg"));
    assert!(Profile::resolve_in(&path.to_string_lossy(), &source, None).is_ok());
}

#[test]
fn container_home_template_rejected() {
    let toml = "image = \"localhost/cistella/opencode:example\"\n\
credential_surface = \"none\"\n\
container_home = \"/home/{{host-home}}\"\n\
mounts = []\n";
    let err = resolve_template_text(toml, Some(ProjectName::Explicit("p"))).unwrap_err();
    assert!(err.to_string().contains("container_home"));
}

#[test]
fn bad_project_charset_rejected_never_sanitized() {
    let toml = template_toml("/data/{{project-name}}", "/x");
    for bad in ["", "../x", "/abs", "a=b", "a b"] {
        let err = resolve_template_text(&toml, Some(ProjectName::Explicit(bad))).unwrap_err();
        assert!(err.to_string().contains("project name"), "for {bad:?}");
    }
    // Dots and dashes are legal (agentmux parity).
    let (prof, _, _) =
        resolve_template_text(&toml, Some(ProjectName::Explicit("my.proj-1"))).unwrap();
    assert_eq!(prof.mounts[0].host_source, "/data/my.proj-1");
}

#[test]
fn adjacent_templates_expand_without_rescan() {
    // Single-pass proof by construction: adjacent spans each expand once;
    // substituted text is never re-examined (see expand_value).
    let toml = template_toml("/{{project-name}}/{{project-name}}", "/x");
    let (prof, _, _) = resolve_template_text(&toml, Some(ProjectName::Explicit("qa"))).unwrap();
    assert_eq!(prof.mounts[0].host_source, "/qa/qa");
    // NOTE: a hostile-$HOME rescan test is deliberately absent: HOME is
    // process-global and mutating it races parallel tests (e.g. the `~`
    // expansion test). The scanner indexes forward over output it just
    // wrote, so rescanning is structurally impossible.
}

#[test]
fn spaced_project_name_ok_without_templates() {
    // Lazy charset validation: template-free profiles never consult the
    // name, so spaced session directories keep working (live parity:
    // spaced_directory_conduct).
    let toml = minimal_toml("localhost/cistella/opencode:example");
    let work = TempDir::new().unwrap();
    let path = work.path().join("plain.toml");
    std::fs::write(&path, toml).unwrap();
    let xdg = TempDir::new().unwrap();
    let source = ResolutionSource::new(Vec::new(), xdg.path().join("xdg"));
    let (prof, _, _) = Profile::resolve_in(
        &path.to_string_lossy(),
        &source,
        Some(ProjectName::Explicit("with space")),
    )
    .unwrap();
    assert_eq!(prof.image, "localhost/cistella/opencode:example");
}

#[test]
fn container_home_expands_canonical_not_raw() {
    // Finding 1: {{container-home}} is the canonical home even when the
    // literal form traverses.
    let toml = "image = \"localhost/cistella/opencode:example\"\n\
credential_surface = \"none\"\n\
container_home = \"/home/cistella/../other\"\n\
[[mounts]]\n\
host_source = \"/data\"\n\
container_target = \"{{container-home}}/x\"\n\
mode = \"ro\"\n";
    let (prof, _, _) = resolve_template_text(toml, Some(ProjectName::Explicit("p"))).unwrap();
    assert_eq!(prof.home(), "/home/other");
    assert_eq!(prof.mounts[0].container_target, "/home/other/x");
}

#[test]
fn unknown_templates_outrank_context_requirements() {
    // Finding 2: exact span recognition — lookalike names report unknown
    // before charset, HOME, or context requirements.
    let lookalike = template_toml("/data/{{not-project-name}}", "/x");
    let err = resolve_template_text(&lookalike, Some(ProjectName::Explicit("p"))).unwrap_err();
    assert!(err.to_string().contains("unknown template"), "{err}");
    let lookalike = template_toml("/data/{{not-host-home}}", "/x");
    let err = resolve_template_text(&lookalike, Some(ProjectName::Explicit("p"))).unwrap_err();
    assert!(err.to_string().contains("unknown template"), "{err}");
    let unknown = template_toml("/data/{{nosuch}}", "/x");
    let err = resolve_template_text(&unknown, None).unwrap_err();
    assert!(err.to_string().contains("unknown template"), "{err}");
}

#[test]
fn root_directory_default_is_lazy() {
    // Finding 3: template-free profiles never derive the basename, so a
    // root session directory resolves; template-bearing ones fail with
    // the basename error (not a generic failure).
    let plain = minimal_toml("localhost/cistella/opencode:example");
    let work = TempDir::new().unwrap();
    let path = work.path().join("plain.toml");
    std::fs::write(&path, plain).unwrap();
    let xdg = TempDir::new().unwrap();
    let source = ResolutionSource::new(Vec::new(), xdg.path().join("xdg"));
    assert!(
        Profile::resolve_in(
            &path.to_string_lossy(),
            &source,
            Some(ProjectName::DirectoryDefault("/")),
        )
        .is_ok()
    );
    let toml = template_toml("/data/{{project-name}}", "/x");
    let err = resolve_template_text(&toml, Some(ProjectName::DirectoryDefault("/"))).unwrap_err();
    assert!(err.to_string().contains("basename"), "{err}");
}

#[test]
fn directory_default_basename_validates_charset() {
    // Tier-2: derived basenames face the same charset gate as explicit
    // names once a {{project-name}} span expands; template-free profiles
    // stay lazy (see root_directory_default_is_lazy).
    let toml = template_toml("/data/{{project-name}}", "/x");
    let err = resolve_template_text(
        &toml,
        Some(ProjectName::DirectoryDefault("/repo/with space")),
    )
    .unwrap_err();
    assert!(err.to_string().contains("project name"), "{err}");
}

fn env_labels_toml(env_value: &str, label_key: &str, label_value: &str) -> String {
    format!(
        "{}\n[env]\nPROBE = \"{env_value}\"\n[labels]\n\"{label_key}\" = \"{label_value}\"\n",
        template_toml("/data", "/x")
    )
}

#[test]
fn env_values_expand_templates() {
    let toml = env_labels_toml(
        "{{container-home}}/.config:{{host-home}}/.x:{{project-name}}",
        "plain",
        "v",
    );
    let home = std::env::var("HOME").unwrap();
    let (profile, _, _) =
        resolve_template_text(&toml, Some(ProjectName::Explicit("proj"))).unwrap();
    assert_eq!(
        profile.env.get("PROBE").map(String::as_str),
        Some(format!("/home/cistella/.config:{home}/.x:proj").as_str())
    );
}

#[test]
fn labels_values_expand_templates() {
    let toml = env_labels_toml("v", "tag", "{{project-name}}-{{container-home}}");
    let (profile, _, _) =
        resolve_template_text(&toml, Some(ProjectName::Explicit("proj"))).unwrap();
    assert_eq!(
        profile.labels.get("tag").map(String::as_str),
        Some("proj-/home/cistella")
    );
}

#[test]
fn unknown_template_in_env_errors() {
    let toml = env_labels_toml("{{bogus}}", "plain", "v");
    let err = resolve_template_text(&toml, Some(ProjectName::Explicit("proj"))).unwrap_err();
    assert!(err.to_string().contains("unknown template"), "{err}");
}

#[test]
fn unterminated_span_in_labels_errors() {
    let toml = env_labels_toml("v", "plain", "{{container-home");
    let err = resolve_template_text(&toml, Some(ProjectName::Explicit("proj"))).unwrap_err();
    assert!(err.to_string().contains("unterminated"), "{err}");
}

#[test]
fn template_in_label_key_errors() {
    let toml = env_labels_toml("v", "{{project-name}}", "v");
    let err = resolve_template_text(&toml, Some(ProjectName::Explicit("proj"))).unwrap_err();
    assert!(err.to_string().contains("label key"), "{err}");
}

#[test]
fn brace_shaped_env_stays_literal_without_context() {
    // Single braces are not spans: template-free profiles pass through
    // with no project context, env and labels included. Built without
    // the command-argv span that template_toml carries.
    let toml = [
        "image = \"localhost/cistella/opencode:example\"",
        "credential_surface = \"none\"",
        "container_home = \"/home/cistella\"",
        "[[mounts]]",
        "host_source = \"/data\"",
        "container_target = \"/x\"",
        "mode = \"rw\"",
        "[env]",
        "PROBE = \"{not-a-span}\"",
        "[labels]",
        "plain = \"(also-literal)\"",
        "",
    ]
    .join("\n");
    let (profile, _, _) = resolve_template_text(&toml, None).unwrap();
    assert_eq!(
        profile.env.get("PROBE").map(String::as_str),
        Some("{not-a-span}")
    );
    assert_eq!(
        profile.labels.get("plain").map(String::as_str),
        Some("(also-literal)")
    );
}

#[test]
fn unterminated_span_hides_env_secret() {
    // Tier-2: diagnostics name the field, never the raw value — env
    // values may carry credentials that must not reach stderr/logs.
    let toml = env_labels_toml("sk-live-SENTINEL-9f8{{", "plain", "v");
    let err = resolve_template_text(&toml, Some(ProjectName::Explicit("proj"))).unwrap_err();
    let msg = err.to_string();
    assert!(!msg.contains("SENTINEL"), "secret leaked: {msg}");
    assert!(msg.contains("env value"), "field named: {msg}");
    assert!(
        !msg.contains("profile: profile:"),
        "doubled display prefix: {msg}"
    );
}

#[test]
fn unterminated_span_hides_label_secret() {
    let toml = env_labels_toml("v", "plain", "tok-SENTINEL-77{{");
    let err = resolve_template_text(&toml, Some(ProjectName::Explicit("proj"))).unwrap_err();
    let msg = err.to_string();
    assert!(!msg.contains("SENTINEL"), "secret leaked: {msg}");
    assert!(msg.contains("label value"), "field named: {msg}");
}
