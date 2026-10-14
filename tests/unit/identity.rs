//! Identity: credential-surface deletion plus the surviving
//! signing helper.
//!
//! Agent forwarding is a declared-extension concern (the shipped
//! ssh guest fed by seat ambient). The slot is gone: profiles
//! naming it refuse at parse.

use cistella::profile::Profile;

#[test]
fn credential_surface_refuses_at_parse() {
    // The deleted slot refuses in both spellings — migration is
    // to declared `ssh`, never to a renamed slot.
    for body in [
        "credential-surface = \"none\"\n",
        "credential-surface = { ssh_agent = \"/run/seat.sock\" }\n",
    ] {
        let toml = format!("image = \"localhost/cistella/opencode:example\"\n{body}mounts = []\n");
        let error = Profile::from_toml(&toml).unwrap_err().to_string();
        assert!(
            error.contains("credential-surface") || error.contains("credential_surface"),
            "slot must refuse naming itself, got: {error}"
        );
    }
}

#[test]
fn minimal_profile_parses_without_surface() {
    let prof = Profile::from_toml(
        r#"
image = "localhost/cistella/opencode:example"
mounts = []
"#,
    )
    .unwrap();
    assert!(prof.environment_acceptances.is_empty());
}

#[test]
fn allowed_signers_format_pins_principal_and_key() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("allowed_signers");
    cistella::identity::write_allowed_signers(&path, "seat", "ssh-ed25519 AAAAC3test").unwrap();
    let content = std::fs::read_to_string(&path).unwrap();
    assert_eq!(content, "seat ssh-ed25519 AAAAC3test\n");
}
