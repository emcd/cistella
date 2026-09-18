//! Identity credential-surface enforcement.

use cistella::profile::Profile;

#[test]
fn none_mounts_nothing_even_with_ambient_sock() {
    // Set ambient SSH_AUTH_SOCK to a fake existing path, but profile is none → zero args.
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("agent.sock");
    std::fs::write(&sock, "").unwrap();
    // SAFETY: set_env is unsafe in this toolchain
    unsafe { std::env::set_var("SSH_AUTH_SOCK", &sock) };
    let prof = Profile::from_toml(
        r#"
image = "localhost/cistella/opencode:example"
credential-surface = "none"
mounts = []
"#,
    )
    .unwrap();
    let args = cistella::identity::ssh_agent_volume_args(&prof);
    assert!(
        args.is_empty(),
        "none should produce zero args, got {args:?}"
    );
    unsafe { std::env::remove_var("SSH_AUTH_SOCK") };
}

#[test]
fn agent_mounts_ro_from_profile() {
    let tmp = tempfile::tempdir().unwrap();
    let sock = tmp.path().join("seat.sock");
    std::fs::write(&sock, "").unwrap();
    let toml = format!(
        r#"
image = "localhost/cistella/opencode:example"
credential-surface = {{ ssh_agent = "{}" }}
mounts = []
"#,
        sock.display()
    );
    let prof = Profile::from_toml(&toml).unwrap();
    let args = cistella::identity::ssh_agent_volume_args(&prof);
    assert_eq!(args.len(), 4);
    assert!(args[1].contains(":ro"));
    assert!(args[3].contains(sock.to_string_lossy().as_ref()));
}

#[test]
fn rejects_github_token_in_env() {
    let prof = Profile::from_toml(
        r#"
image = "localhost/cistella/opencode:example"
credential-surface = "none"
mounts = []
[environment]
GITHUB_TOKEN = "secret"
"#,
    )
    .unwrap();
    assert!(cistella::identity::assert_no_github_token(&prof).is_err());
}
