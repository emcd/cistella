//! Conduct-side lattice evaluation of profile contributions (task 4.1).
//!
//! Deliberate migration pins: the legacy unconditional
//! token-assignments veto is gone; token-shaped assignment names now
//! refuse as `suppressible × universal` (absent user policy file
//! included) and pass only with exact-name acknowledgement. Shipped
//! acceptances grandfather against compiled defaults only; user rules
//! take precedence; fleet names pass untouched under defaults.

use cistella::framework::conduct::evaluate_profile_contributions;
use cistella::framework::policy::PolicySet;
use cistella::profile::Profile;

fn minimal_toml(body: &str) -> String {
    format!(
        "image = \"localhost/cistella/opencode:example\"\ncredential-surface = \"none\"\nmounts = []\n{body}"
    )
}

/// Compiled defaults via load on an empty dir: absent file means
/// defaults only (pins the unchanged-defaults refusal path, not just
/// a hand-built default set).
fn defaults_via_absent_file() -> (tempfile::TempDir, PolicySet) {
    let dir = tempfile::tempdir().expect("tempdir");
    let policy = PolicySet::load(Some(dir.path())).expect("absent file means defaults");
    (dir, policy)
}

#[test]
fn assignment_token_without_ack_refuses() {
    let prof = Profile::from_toml(&minimal_toml(
        "[environment-assignments]\nGITHUB_TOKEN = \"secret\"\n",
    ))
    .unwrap();
    let (_dir, policy) = defaults_via_absent_file();
    let error = evaluate_profile_contributions(&prof, &[], &policy)
        .expect_err("unacknowledged token assignment must refuse");
    let message = error.to_string();
    assert!(
        message.contains("GITHUB_TOKEN"),
        "diagnostic names the variable, got: {message}"
    );
    assert!(
        !message.contains("secret"),
        "diagnostic must never render the value, got: {message}"
    );
}

#[test]
fn assignment_token_with_exact_ack_passes() {
    let prof = Profile::from_toml(&minimal_toml(
        "[environment-assignments]\nGITHUB_TOKEN = \"secret\"\n",
    ))
    .unwrap();
    let policy =
        PolicySet::parse(b"format_version = 1\n[[acknowledgements]]\nname = \"GITHUB_TOKEN\"\n")
            .expect("acknowledgement parses");
    evaluate_profile_contributions(&prof, &[], &policy)
        .expect("exact acknowledgement excuses the suppressible default");
}

#[test]
fn accepted_token_name_grandfathered_against_defaults_only() {
    let prof = Profile::from_toml(&minimal_toml(
        "environment-acceptances = [\"GITHUB_TOKEN\"]\n",
    ))
    .unwrap();
    let (_dir, policy) = defaults_via_absent_file();
    let accepted = vec![("GITHUB_TOKEN".to_string(), "invoker-value".to_string())];
    evaluate_profile_contributions(&prof, &accepted, &policy)
        .expect("shipped acceptance grandfathers against the compiled default");
}

#[test]
fn user_denial_over_accepted_name_refuses() {
    let prof = Profile::from_toml(&minimal_toml(
        "environment-acceptances = [\"AGENTMUX_BUNDLE\"]\n",
    ))
    .unwrap();
    let policy = PolicySet::parse(
        "format_version = 1\n[[denials]]\npattern = \"AGENTMUX_.*\"\nseverity = \"suppressible\"\nscope = \"universal\"\n".as_bytes(),
    )
    .expect("user denial parses");
    let accepted = vec![("AGENTMUX_BUNDLE".to_string(), "seat-7f3a".to_string())];
    let error = evaluate_profile_contributions(&prof, &accepted, &policy)
        .expect_err("user rules take precedence over grandfathering");
    assert!(
        error.to_string().contains("AGENTMUX_BUNDLE"),
        "diagnostic names the variable, got: {error}"
    );
}

#[test]
fn fleet_names_pass_under_defaults() {
    let prof = Profile::from_toml(&minimal_toml(
        "environment-acceptances = [\"AGENTMUX_BUNDLE\", \"AGENTMUX_SESSION\"]\n[environment-assignments]\nOPCODE_IMAGE_TAG = \"example\"\n",
    ))
    .unwrap();
    let (_dir, policy) = defaults_via_absent_file();
    let accepted = vec![
        ("AGENTMUX_BUNDLE".to_string(), "seat-7f3a".to_string()),
        ("AGENTMUX_SESSION".to_string(), "session-9c1e".to_string()),
    ];
    evaluate_profile_contributions(&prof, &accepted, &policy)
        .expect("fleet contributions pass identically under defaults");
}
