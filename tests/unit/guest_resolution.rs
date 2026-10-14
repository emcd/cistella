//! Profile guest resolution: shipped-first admission, registry tables,
//! wrong-slot refusals, and closed-admission preservation.
//!
//! Split from `profile_resolution` at the file-size limit: profile
//! parsing and templates stay there, guest selection lives here.

use cistella::guest_registry::Registry;

#[test]
fn guest_tables_default_plain() {
    use cistella::guest_registry::Registry;
    use cistella::profile::IsolatorResolution;
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("plain.toml");
    std::fs::write(
        &path,
        "image = \"localhost/cistella/opencode:example\"\nmounts = []\n",
    )
    .unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    let prof: cistella::profile::Profile = toml::from_str(&text).expect("minimal parses");
    // Absent tables: podman backend, no guests — 0.1.x profiles
    // run unchanged on the plain 2.x path.
    assert_eq!(
        prof.resolve_isolator(&Registry::default()).unwrap(),
        IsolatorResolution::Podman
    );
    assert!(
        prof.resolve_extensions(&Registry::default())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn guest_tables_admit_declared_landlock() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("hooked.toml");
    std::fs::write(
        &path,
        "image = \"localhost/cistella/opencode:example\"\n\
         mounts = []\n\
         [isolator]\n\
         name = \"podman\"\n\
         [[extensions]]\n\
         name = \"landlock\"\n",
    )
    .unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    let prof: cistella::profile::Profile = toml::from_str(&text).expect("tables parse");
    use cistella::guest_registry::Registry;
    use cistella::profile::{ExtensionResolution, IsolatorResolution};
    assert_eq!(
        prof.resolve_isolator(&Registry::default()).unwrap(),
        IsolatorResolution::Podman
    );
    assert_eq!(
        prof.resolve_extensions(&Registry::default()).unwrap(),
        vec![ExtensionResolution::Landlock]
    );
}

#[test]
fn guest_tables_refuse_unknown_and_duplicate() {
    // Unknown isolator refuses naming it.
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("bad-iso.toml");
    std::fs::write(
        &path,
        "image = \"localhost/cistella/opencode:example\"\n\
         mounts = []\n\
         [isolator]\n\
         name = \"docker\"\n",
    )
    .unwrap();
    let prof: cistella::profile::Profile =
        toml::from_str(&std::fs::read_to_string(&path).unwrap()).expect("parses");
    let error = prof.resolve_isolator(&Registry::default()).unwrap_err();
    assert!(error.to_string().contains("docker"), "got: {error}");
    // Unknown extension refuses (a typo can never silently
    // plain a meant-hooked session); duplicates refuse.
    for (name, toml_name, expect) in [
        ("landlok", "bad-ext.toml", "landlok"),
        ("landlock", "dup-ext.toml", "duplicate"),
    ] {
        let path = dir.path().join(toml_name);
        let second = if expect == "duplicate" {
            "         [[extensions]]\n         name = \"landlock\"\n"
        } else {
            ""
        };
        std::fs::write(
            &path,
            format!(
                "image = \"localhost/cistella/opencode:example\"\n\
                 mounts = []\n\
                 [[extensions]]\n\
                 name = \"{name}\"\n{second}"
            ),
        )
        .unwrap();
        let prof: cistella::profile::Profile =
            toml::from_str(&std::fs::read_to_string(&path).unwrap()).expect("parses");
        let error = prof.resolve_extensions(&Registry::default()).unwrap_err();
        assert!(error.to_string().contains(expect), "got: {error}");
    }
}

fn test_registry() -> Registry {
    Registry::parse(
        "format-version = 1\n\
         [[isolators]]\n\
         name = \"file-isolator\"\n\
         role = \"isolator\"\n\
         protocol = 1\n\
         source-digest = { algorithm = 'sha256', value = \"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855\" }\n\
         source-path = \"/opt/guests/isolator\"\n\
         [[extensions]]\n\
         name = \"file-extension\"\n\
         role = \"fileext\"\n\
         protocol = 1\n\
         source-digest = { algorithm = 'sha256', value = \"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855\" }\n\
         source-path = \"/opt/guests/extension\"\n",
    )
    .expect("test registry parses")
}

fn profile_naming(isolator: &str, extensions: &[&str]) -> cistella::profile::Profile {
    let mut toml = format!(
        "image = \"localhost/cistella/opencode:example\"\n\
         mounts = []\n\
         [isolator]\n\
         name = \"{isolator}\"\n"
    );
    for name in extensions {
        toml.push_str(&format!("[[extensions]]\nname = \"{name}\"\n"));
    }
    toml::from_str(&toml).expect("profile parses")
}

#[test]
fn registered_names_resolve_by_table() {
    use cistella::profile::{ExtensionResolution, IsolatorResolution};
    let registry = test_registry();
    let prof = profile_naming("file-isolator", &["file-extension"]);
    match prof.resolve_isolator(&registry).expect("isolator resolves") {
        IsolatorResolution::Registered(entry) => assert_eq!(entry.name, "file-isolator"),
        IsolatorResolution::Podman => panic!("must resolve registered"),
    }
    let extensions = prof
        .resolve_extensions(&registry)
        .expect("extension resolves");
    assert_eq!(extensions.len(), 1);
    match &extensions[0] {
        ExtensionResolution::Registered(entry) => assert_eq!(entry.name, "file-extension"),
        ExtensionResolution::Landlock | ExtensionResolution::Tmpdir | ExtensionResolution::Ssh => {
            panic!("must resolve registered")
        }
    }
    // Shipped names resolve first even with a populated registry.
    let prof = profile_naming("podman", &["landlock"]);
    assert_eq!(
        prof.resolve_isolator(&registry).unwrap(),
        IsolatorResolution::Podman
    );
    assert_eq!(
        prof.resolve_extensions(&registry).unwrap(),
        vec![ExtensionResolution::Landlock]
    );
    // Tmpdir resolves shipped with no registry entry.
    let prof = profile_naming("podman", &["tmpdir"]);
    assert_eq!(
        prof.resolve_extensions(&Registry::default()).unwrap(),
        vec![ExtensionResolution::Tmpdir]
    );
}

#[test]
fn wrong_slot_names_refuse_as_slot_errors() {
    let registry = test_registry();
    // Extension entry in the isolator slot.
    let prof = profile_naming("file-extension", &[]);
    let error = prof
        .resolve_isolator(&registry)
        .expect_err("wrong slot must refuse");
    assert!(
        error.to_string().contains("registered as an extension"),
        "got: {error}"
    );
    // Isolator entry in an extension slot.
    let prof = profile_naming("podman", &["file-isolator"]);
    let error = prof
        .resolve_extensions(&registry)
        .expect_err("wrong slot must refuse");
    assert!(
        error.to_string().contains("registered as an isolator"),
        "got: {error}"
    );
}

#[test]
fn unknown_names_refuse_with_populated_registry() {
    let registry = test_registry();
    let prof = profile_naming("docker", &[]);
    let error = prof
        .resolve_isolator(&registry)
        .expect_err("unknown must refuse");
    assert!(
        error.to_string().contains("unknown isolator"),
        "got: {error}"
    );
    let prof = profile_naming("podman", &["landlok"]);
    let error = prof
        .resolve_extensions(&registry)
        .expect_err("unknown must refuse");
    assert!(error.to_string().contains("landlok"), "got: {error}");
    // Duplicate registered extensions refuse by declared name.
    let prof = profile_naming("podman", &["file-extension", "file-extension"]);
    let error = prof
        .resolve_extensions(&registry)
        .expect_err("duplicate must refuse");
    assert!(error.to_string().contains("duplicate"), "got: {error}");
}
