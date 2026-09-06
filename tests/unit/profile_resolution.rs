//! Profile resolution: tiered lookup, closed supplied tiers, seed-if-absent.
//!
//! All fixtures use tempdirs passed explicitly as lookup inputs, so no test
//! touches the real cwd, `XDG_CONFIG_HOME`, or `HOME` — except
//! `env_tier_end_to_end`, the single test that mutates process env, which
//! saves and restores every var it sets.

use std::path::{Path, PathBuf};

use tempfile::TempDir;

use cistella::profile::{Profile, ResolutionSource};

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
    let (prof, digest, name) = Profile::resolve_in("default", &source).unwrap();
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
    let (prof, _digest, name) = Profile::resolve_in("opencode", &source).unwrap();
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
    let (prof, _, _) = Profile::resolve_in("custom", &source).unwrap();
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
    let (prof, _, _) = Profile::resolve_in("custom", &source).unwrap();
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
    let err = Profile::resolve_in("custom", &source).unwrap_err();
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
    let err = Profile::resolve_in("default", &source).unwrap_err();
    assert!(err.to_string().contains("default"));
    assert!(!xdg.exists(), "closed tier never seeds");
}

#[test]
fn unknown_name_errors_and_still_seeds_baked() {
    let xdg_base = TempDir::new().unwrap();
    let (source, xdg) = fresh_source(&xdg_base);
    let err = Profile::resolve_in("nope", &source).unwrap_err();
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
    let (prof, _digest, name) = Profile::resolve_in(&path.to_string_lossy(), &source).unwrap();
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
    let err = Profile::resolve_in(&missing.to_string_lossy(), &source).unwrap_err();
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
        Profile::resolve_in("custom", &source)
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
