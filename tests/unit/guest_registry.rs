//! Seat-local guest registry: schema, XDG/HOME location rules,
//! anchored file reads, and entry validation.
//!
//! Parse-level tests pin the entry key set (one positive
//! all-required-keys case, one negative unknown-key case) plus every
//! refusal shape; file-level tests pin the anchored-read contract
//! (absent means empty, symlink/mode refusals, both swap cases).

use std::os::unix::fs::PermissionsExt;

use cistella::guest_registry::{GuestTable, HostEnv, Registry, begin_anchored, registry_path};

/// Well-formed 64-lowercase-hex digest shape (content irrelevant at
/// parse level — verification compares at resolve).
const DIGEST: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

fn valid_registry() -> String {
    format!(
        "format-version = 1\n\
         [[isolators]]\n\
         name = \"file-isolator\"\n\
         role = \"isolator\"\n\
         protocol = 1\n\
         sha256 = \"{DIGEST}\"\n\
         path = \"/opt/guests/isolator\"\n\
         args = [\"--flag\"]\n\
         [[extensions]]\n\
         name = \"net-extension\"\n\
         role = \"netext\"\n\
         protocol = 1\n\
         sha256 = \"{DIGEST}\"\n\
         url = \"https://example.invalid/guest\"\n"
    )
}

#[test]
fn positive_all_required_keys_parses() {
    let registry = Registry::parse(&valid_registry()).expect("valid registry parses");
    let isolator = registry
        .find(GuestTable::Isolators, "file-isolator")
        .expect("isolator present");
    assert_eq!(isolator.role, "isolator");
    assert_eq!(isolator.protocol, 1);
    assert_eq!(isolator.args, vec!["--flag".to_string()]);
    assert!(matches!(
        isolator.source,
        cistella::guest_registry::GuestSource::Path { .. }
    ));
    let extension = registry
        .find(GuestTable::Extensions, "net-extension")
        .expect("extension present");
    assert_eq!(extension.role, "netext");
    assert!(matches!(
        extension.source,
        cistella::guest_registry::GuestSource::Fetch { .. }
    ));
    assert!(
        registry
            .find(GuestTable::Isolators, "net-extension")
            .is_none()
    );
}

#[test]
fn negative_unknown_keys_refuse() {
    let text = valid_registry() + "unknown-top = true\n";
    let error = Registry::parse(&text).expect_err("unknown top-level key must refuse");
    assert!(error.to_string().contains("registry parse"), "got: {error}");
    let text = format!(
        "format-version = 1\n\
         [[extensions]]\n\
         name = \"x\"\n\
         role = \"x\"\n\
         protocol = 1\n\
         sha256 = \"{DIGEST}\"\n\
         path = \"/opt/guests/x\"\n\
         signature = \"deadbeef\"\n"
    );
    let error = Registry::parse(&text).expect_err("unknown entry key must refuse");
    assert!(error.to_string().contains("registry parse"), "got: {error}");
}

#[test]
fn format_version_absent_or_otherwise_refuses() {
    let text = format!(
        "[[extensions]]\n\
         name = \"x\"\n\
         role = \"x\"\n\
         protocol = 1\n\
         sha256 = \"{DIGEST}\"\n\
         path = \"/opt/guests/x\"\n"
    );
    let error = Registry::parse(&text).expect_err("absent version must refuse");
    assert!(error.to_string().contains("format-version"), "got: {error}");
    let text = valid_registry().replace("format-version = 1", "format-version = 2");
    let error = Registry::parse(&text).expect_err("other version must refuse");
    assert!(error.to_string().contains("format-version"), "got: {error}");
}

#[test]
fn malformed_toml_refuses() {
    let error = Registry::parse("format-version = [unclosed").expect_err("malformed must refuse");
    assert!(error.to_string().contains("registry parse"), "got: {error}");
}

#[test]
fn shadowing_reserved_names_refuses_at_parse() {
    for (table, name) in [("isolators", "podman"), ("extensions", "landlock")] {
        let text = format!(
            "format-version = 1\n\
             [[{table}]]\n\
             name = \"{name}\"\n\
             role = \"{name}\"\n\
             protocol = 1\n\
             sha256 = \"{DIGEST}\"\n\
             path = \"/opt/guests/{name}\"\n"
        );
        let error = Registry::parse(&text).expect_err("shadowing entry must refuse");
        assert!(
            error.to_string().contains("shadows shipped name"),
            "got: {error}"
        );
    }
}

#[test]
fn landlock_role_in_isolators_stays_legal_for_future_sibling() {
    let text = format!(
        "format-version = 1\n\
         [[isolators]]\n\
         name = \"future-landlock\"\n\
         role = \"landlock\"\n\
         protocol = 1\n\
         sha256 = \"{DIGEST}\"\n\
         path = \"/opt/guests/future\"\n"
    );
    let registry = Registry::parse(&text).expect("landlock role in isolators parses");
    assert!(
        registry
            .find(GuestTable::Isolators, "future-landlock")
            .is_some()
    );
}

#[test]
fn podman_role_in_extensions_refuses_at_parse() {
    let text = format!(
        "format-version = 1\n\
         [[extensions]]\n\
         name = \"fake-podman\"\n\
         role = \"podman\"\n\
         protocol = 1\n\
         sha256 = \"{DIGEST}\"\n\
         path = \"/opt/guests/fake\"\n"
    );
    let error = Registry::parse(&text).expect_err("podman role in extensions must refuse");
    assert!(
        error.to_string().contains("reserved isolator role"),
        "got: {error}"
    );
}

#[test]
fn duplicate_name_within_table_refuses() {
    let text = format!(
        "format-version = 1\n\
         [[extensions]]\n\
         name = \"dup\"\n\
         role = \"a\"\n\
         protocol = 1\n\
         sha256 = \"{DIGEST}\"\n\
         path = \"/opt/guests/a\"\n\
         [[extensions]]\n\
         name = \"dup\"\n\
         role = \"b\"\n\
         protocol = 1\n\
         sha256 = \"{DIGEST}\"\n\
         path = \"/opt/guests/b\"\n"
    );
    let error = Registry::parse(&text).expect_err("duplicate must refuse");
    assert!(error.to_string().contains("duplicate"), "got: {error}");
}

#[test]
fn companion_pair_across_tables_is_legal() {
    let text = format!(
        "format-version = 1\n\
         [[isolators]]\n\
         name = \"companion\"\n\
         role = \"landlock\"\n\
         protocol = 1\n\
         sha256 = \"{DIGEST}\"\n\
         path = \"/opt/guests/iso\"\n\
         [[extensions]]\n\
         name = \"companion\"\n\
         role = \"landlock\"\n\
         protocol = 1\n\
         sha256 = \"{DIGEST}\"\n\
         path = \"/opt/guests/ext\"\n"
    );
    let registry = Registry::parse(&text).expect("companion pair parses");
    let (table, _) = registry.find_any("companion").expect("found");
    assert_eq!(table, GuestTable::Isolators);
}

#[test]
fn empty_name_and_role_refuse() {
    let text = format!(
        "format-version = 1\n\
         [[extensions]]\n\
         name = \"\"\n\
         role = \"x\"\n\
         protocol = 1\n\
         sha256 = \"{DIGEST}\"\n\
         path = \"/opt/guests/x\"\n"
    );
    let error = Registry::parse(&text).expect_err("empty name must refuse");
    assert!(
        error.to_string().contains("must not be empty"),
        "got: {error}"
    );
    let text = valid_registry().replace("role = \"netext\"", "role = \"\"");
    let error = Registry::parse(&text).expect_err("empty role must refuse");
    assert!(
        error.to_string().contains("must not be empty"),
        "got: {error}"
    );
}

fn spanned_entry(name: &str, role: &str, source: &str, args: &str) -> String {
    format!(
        "format-version = 1\n\
         [[extensions]]\n\
         name = \"{name}\"\n\
         role = \"{role}\"\n\
         protocol = 1\n\
         sha256 = \"{DIGEST}\"\n\
         {source}\n\
         {args}\n"
    )
}

#[test]
fn template_spans_refuse_in_every_field() {
    let cases = [
        spanned_entry(
            "{{supplement:x}}",
            "spanned",
            "url = \"https://example.invalid/guest\"",
            "",
        ),
        spanned_entry(
            "spanned",
            "{{env:y}}",
            "url = \"https://example.invalid/guest\"",
            "",
        ),
        spanned_entry(
            "spanned",
            "spanned",
            "url = \"https://example.invalid/{{z}}\"",
            "",
        ),
        spanned_entry("spanned", "spanned", "path = \"/opt/{{w}}\"", ""),
        spanned_entry(
            "spanned",
            "spanned",
            "url = \"https://example.invalid/guest\"",
            "args = [\"--x={{v}}\"]",
        ),
    ];
    for text in cases {
        let error = Registry::parse(&text).expect_err("span must refuse");
        assert!(
            error.to_string().contains("template spans"),
            "got: {error} for {text}"
        );
    }
}

#[test]
fn source_shapes_refuse_at_parse() {
    // Non-http(s) fetch URL.
    let text = valid_registry().replace(
        "https://example.invalid/guest",
        "ftp://example.invalid/guest",
    );
    let error = Registry::parse(&text).expect_err("ftp url must refuse");
    assert!(error.to_string().contains("http(s)"), "got: {error}");
    // Relative path.
    let text = valid_registry().replace("/opt/guests/isolator", "relative/isolator");
    let error = Registry::parse(&text).expect_err("relative path must refuse");
    assert!(error.to_string().contains("absolute"), "got: {error}");
    // Both sources.
    let text = valid_registry().replace(
        "path = \"/opt/guests/isolator\"",
        "path = \"/opt/guests/isolator\"\nurl = \"https://example.invalid/dup\"",
    );
    let error = Registry::parse(&text).expect_err("dual source must refuse");
    assert!(
        error.to_string().contains("exactly one source"),
        "got: {error}"
    );
    // Neither source.
    let text = valid_registry().replace("path = \"/opt/guests/isolator\"\n", "");
    let error = Registry::parse(&text).expect_err("missing source must refuse");
    assert!(
        error.to_string().contains("declare a source"),
        "got: {error}"
    );
    // Malformed digests.
    for bad in ["xyz", &"AB".repeat(32), &"0".repeat(63)] {
        let text = valid_registry().replace(DIGEST, bad);
        let error = Registry::parse(&text).expect_err("bad digest must refuse");
        assert!(
            error.to_string().contains("sha256"),
            "digest {bad} got: {error}"
        );
    }
}

#[test]
fn registry_path_location_rules() {
    // XDG set: direct.
    let env = HostEnv::new(Some("/seat/config"), Some("/home/seat"));
    assert_eq!(
        registry_path(&env).expect("xdg path"),
        std::path::PathBuf::from("/seat/config/cistella/registry.toml")
    );
    // XDG unset or empty: HOME fallback.
    for xdg in [None, Some("")] {
        let env = HostEnv::new(xdg, Some("/home/seat"));
        assert_eq!(
            registry_path(&env).expect("fallback path"),
            std::path::PathBuf::from("/home/seat/.config/cistella/registry.toml")
        );
    }
    // Relative XDG refuses (never cwd-relative).
    let env = HostEnv::new(Some("config"), Some("/home/seat"));
    let error = registry_path(&env).expect_err("relative XDG must refuse");
    assert!(error.to_string().contains("absolute"), "got: {error}");
    // Unset or empty HOME refuses.
    for home in [None, Some("")] {
        let env = HostEnv::new(None, home);
        let error = registry_path(&env).expect_err("missing HOME must refuse");
        assert!(error.to_string().contains("HOME"), "got: {error}");
    }
}

fn write_registry(dir: &tempfile::TempDir, name: &str, text: &str) -> std::path::PathBuf {
    let path = dir.path().join(name);
    std::fs::write(&path, text).expect("write fixture");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    path
}

#[test]
fn absent_file_loads_empty() {
    let dir = tempfile::tempdir().expect("tempdir");
    let registry =
        Registry::load_from_path(&dir.path().join("registry.toml")).expect("absent loads");
    assert!(registry.is_empty());
}

#[test]
fn symlink_registry_refuses_unfollowed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let real = write_registry(&dir, "registry.toml", &valid_registry());
    let link = dir.path().join("link.toml");
    std::os::unix::fs::symlink(&real, &link).expect("symlink");
    let error = Registry::load_from_path(&link).expect_err("symlink must refuse");
    assert!(error.to_string().contains("symlink"), "got: {error}");
}

#[test]
fn group_writable_registry_refuses() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_registry(&dir, "registry.toml", &valid_registry());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o664)).expect("chmod");
    let error = Registry::load_from_path(&path).expect_err("group-writable must refuse");
    assert!(
        error.to_string().contains("group/world-writable"),
        "got: {error}"
    );
}

#[test]
fn swap_before_open_validates_the_new_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_registry(&dir, "registry.toml", &valid_registry());
    Registry::load_from_path(&path).expect("valid loads");
    std::fs::write(&path, "format-version = [unclosed").expect("overwrite");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    let error = Registry::load_from_path(&path).expect_err("replacement validates anew");
    assert!(error.to_string().contains("registry parse"), "got: {error}");
}

#[test]
fn pathname_swap_after_open_parses_pinned_original() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_registry(&dir, "registry.toml", &valid_registry());
    let open = begin_anchored(&path, 1024 * 1024)
        .expect("open anchors")
        .expect("present");
    // Replace the path after open: the pinned FD still names the
    // original bytes, so finish parses the original and proceeds.
    let swap = dir.path().join("swap.toml");
    std::fs::write(&swap, "format-version = [unclosed").expect("write swap");
    std::fs::set_permissions(&swap, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    std::fs::rename(&swap, &path).expect("swap over path");
    let anchored = open.finish().expect("pinned read proceeds");
    let text = std::str::from_utf8(&anchored.bytes).expect("utf-8");
    let registry = Registry::parse(text).expect("pinned original parses");
    assert!(
        registry
            .find(GuestTable::Extensions, "net-extension")
            .is_some()
    );
    // The replacement is admitted only at the next conduct, when it
    // is eagerly read and revalidated — and it refuses.
    let error = Registry::load_from_path(&path).expect_err("replacement refuses anew");
    assert!(error.to_string().contains("registry parse"), "got: {error}");
}

#[test]
fn in_place_mode_change_during_read_refuses() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_registry(&dir, "registry.toml", &valid_registry());
    let open = begin_anchored(&path, 1024 * 1024)
        .expect("open anchors")
        .expect("present");
    // In-place mode change between open-fstat and post-read fstat.
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).expect("chmod");
    let error = open.finish().expect_err("mode change must refuse");
    assert!(
        error.to_string().contains("changed owner or mode"),
        "got: {error}"
    );
}

/// Computes the registry-shape digest of fixture bytes (test
/// visibility into the same sha2 version src verifies with).
fn fixture_digest(bytes: &[u8]) -> [u8; 32] {
    use sha2::Digest;
    sha2::Sha256::digest(bytes).into()
}

fn write_guest(dir: &tempfile::TempDir, name: &str, mode: u32) -> (std::path::PathBuf, Vec<u8>) {
    let bytes = b"#!/bin/sh\nexit 0\n".to_vec();
    let path = dir.path().join(name);
    std::fs::write(&path, &bytes).expect("write fixture");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("chmod");
    (path, bytes)
}

#[test]
fn path_source_verifies_and_pins() {
    use cistella::guest_registry::resolve_path_source;
    let dir = tempfile::tempdir().expect("tempdir");
    let (path, bytes) = write_guest(&dir, "guest", 0o755);
    let pinned = resolve_path_source(&path, &fixture_digest(&bytes)).expect("valid resolves");
    assert_eq!(pinned.digest, fixture_digest(&bytes));
}

#[test]
fn replaced_path_guest_refuses_at_conduct() {
    use cistella::guest_registry::resolve_path_source;
    let dir = tempfile::tempdir().expect("tempdir");
    let (path, bytes) = write_guest(&dir, "guest", 0o755);
    let expected = fixture_digest(&bytes);
    resolve_path_source(&path, &expected).expect("original resolves");
    std::fs::write(&path, b"#!/bin/sh\nexit 1\n").expect("replace bytes");
    let error = resolve_path_source(&path, &expected).expect_err("replacement must refuse");
    assert!(
        error.to_string().contains("digest mismatch"),
        "got: {error}"
    );
}

#[test]
fn relative_registered_path_refuses() {
    use cistella::guest_registry::resolve_path_source;
    let error = resolve_path_source(
        std::path::Path::new("relative/guest"),
        &fixture_digest(b"x"),
    )
    .expect_err("relative must refuse");
    assert!(error.to_string().contains("absolute"), "got: {error}");
}

#[test]
fn symlink_final_registered_path_refuses() {
    use cistella::guest_registry::resolve_path_source;
    let dir = tempfile::tempdir().expect("tempdir");
    let (real, bytes) = write_guest(&dir, "real", 0o755);
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&real, &link).expect("symlink");
    let error =
        resolve_path_source(&link, &fixture_digest(&bytes)).expect_err("symlink must refuse");
    assert!(error.to_string().contains("symlink"), "got: {error}");
}

#[test]
fn non_executable_registered_path_refuses() {
    use cistella::guest_registry::resolve_path_source;
    let dir = tempfile::tempdir().expect("tempdir");
    let (path, bytes) = write_guest(&dir, "guest", 0o644);
    let error =
        resolve_path_source(&path, &fixture_digest(&bytes)).expect_err("non-exec must refuse");
    assert!(error.to_string().contains("not executable"), "got: {error}");
}

#[test]
fn group_writable_registered_path_refuses() {
    use cistella::guest_registry::resolve_path_source;
    let dir = tempfile::tempdir().expect("tempdir");
    let (path, bytes) = write_guest(&dir, "guest", 0o775);
    let error = resolve_path_source(&path, &fixture_digest(&bytes))
        .expect_err("group-writable must refuse");
    assert!(
        error.to_string().contains("group/world-writable"),
        "got: {error}"
    );
}

#[test]
fn symlinked_parent_canonicalizes() {
    use cistella::guest_registry::resolve_path_source;
    let dir = tempfile::tempdir().expect("tempdir");
    let real = dir.path().join("real");
    std::fs::create_dir(&real).expect("mkdir");
    let bytes = b"#!/bin/sh\nexit 0\n".to_vec();
    std::fs::write(real.join("guest"), &bytes).expect("write fixture");
    std::fs::set_permissions(real.join("guest"), std::fs::Permissions::from_mode(0o755))
        .expect("chmod");
    std::os::unix::fs::symlink(&real, dir.path().join("link")).expect("symlink");
    let pinned = resolve_path_source(&dir.path().join("link/guest"), &fixture_digest(&bytes))
        .expect("symlinked parent resolves");
    assert_eq!(pinned.digest, fixture_digest(&bytes));
}

#[test]
fn world_writable_ancestor_refuses_sticky_passes() {
    use cistella::guest_registry::resolve_path_source;
    let dir = tempfile::tempdir().expect("tempdir");
    for (name, mode, ok) in [("open", 0o777, false), ("sticky", 0o1777, true)] {
        let sub = dir.path().join(name);
        std::fs::create_dir(&sub).expect("mkdir");
        let bytes = b"#!/bin/sh\nexit 0\n".to_vec();
        std::fs::write(sub.join("guest"), &bytes).expect("write fixture");
        std::fs::set_permissions(sub.join("guest"), std::fs::Permissions::from_mode(0o755))
            .expect("chmod");
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(mode)).expect("chmod");
        let outcome = resolve_path_source(&sub.join("guest"), &fixture_digest(&bytes));
        if ok {
            outcome.expect("sticky ancestor passes");
        } else {
            let error = outcome.expect_err("world-writable ancestor must refuse");
            assert!(error.to_string().contains("world-writable"), "got: {error}");
        }
        // Restore owner-write access for tempdir cleanup on strict umasks.
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
}

#[test]
fn missing_registered_path_refuses_typed() {
    use cistella::guest_registry::resolve_path_source;
    let dir = tempfile::tempdir().expect("tempdir");
    let error = resolve_path_source(&dir.path().join("absent"), &fixture_digest(b"x"))
        .expect_err("absence must refuse");
    assert!(error.to_string().contains("absent"), "got: {error}");
    let error = resolve_path_source(
        std::path::Path::new("/nonexistent-dir-cistella/guest"),
        &fixture_digest(b"x"),
    )
    .expect_err("missing parent must refuse");
    assert!(error.to_string().contains("absent"), "got: {error}");
}
