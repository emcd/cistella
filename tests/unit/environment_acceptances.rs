//! `environment-acceptances`: declaration, requiredness, collisions,
//! verbatim values, safety gate, and no-render-permission.
//!
//! Tests that set process env use unique `CISTELLA_ACC_*` names per test
//! and restore prior state on drop, so parallel runners never interact.

use tempfile::TempDir;

use cistella::profile::{Profile, ProjectName, ResolutionSource, Supplements};
use cistella::session::mint_session_id;

/// Saves and restores process env around a test.
struct EnvGuard {
    saved: Vec<(String, Option<String>)>,
}

impl EnvGuard {
    fn set(names: &[(&str, &str)]) -> Self {
        let saved = names
            .iter()
            .map(|(k, v)| {
                let prev = std::env::var(k).ok();
                // NUL is unrepresentable in OS env; test bug if attempted.
                assert!(!v.contains('\0'));
                unsafe { std::env::set_var(k, v) };
                (k.to_string(), prev)
            })
            .collect();
        Self { saved }
    }

    fn remove(names: &[&str]) -> Self {
        let saved = names
            .iter()
            .map(|k| {
                let prev = std::env::var(k).ok();
                unsafe { std::env::remove_var(k) };
                (k.to_string(), prev)
            })
            .collect();
        Self { saved }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (k, prev) in self.saved.drain(..) {
            match prev {
                Some(v) => unsafe { std::env::set_var(&k, v) },
                None => unsafe { std::env::remove_var(&k) },
            }
        }
    }
}

fn minimal_toml(body: &str) -> String {
    format!(
        "image = \"localhost/cistella/opencode:example\"\ncredential-surface = \"none\"\nmounts = []\n{body}"
    )
}

fn profile_with_acceptances(names: &[&str]) -> String {
    let list = names
        .iter()
        .map(|n| format!("\"{n}\""))
        .collect::<Vec<_>>()
        .join(", ");
    minimal_toml(&format!("environment-acceptances = [{list}]\n"))
}

fn resolve_text(toml: &str) -> Result<Profile, cistella::error::CistellaError> {
    let work = TempDir::new().unwrap();
    let path = work.path().join("tmpl.toml");
    std::fs::write(&path, toml).unwrap();
    let xdg_base = TempDir::new().unwrap();
    let source = ResolutionSource::new(Vec::new(), xdg_base.path().join("xdg"));
    Profile::resolve_in(
        path.to_str().unwrap(),
        &source,
        Some(ProjectName::Explicit("proj")),
        &Supplements::default(),
    )
    .map(|(p, _, _)| p)
}

#[test]
fn legacy_environment_table_fails_closed() {
    let bad = minimal_toml("[environment]\nPROBE = \"x\"\n");
    assert!(Profile::from_toml(&bad).is_err());
}

#[test]
fn declaration_valid_list_parses() {
    let prof = Profile::from_toml(&profile_with_acceptances(&["CISTELLA_ACC_T1"]))
        .expect("valid acceptance list parses");
    assert_eq!(prof.environment_acceptances, vec!["CISTELLA_ACC_T1"]);
}

#[test]
fn declaration_duplicate_names_fail() {
    let err = Profile::from_toml(&profile_with_acceptances(&[
        "CISTELLA_ACC_DUP",
        "CISTELLA_ACC_DUP",
    ]))
    .expect_err("duplicate acceptance names refuse");
    assert!(err.to_string().contains("CISTELLA_ACC_DUP"));
}

#[test]
fn declaration_bad_grammar_fails() {
    for bad in ["cistella_acc_lower", "9LIVES", "HAS-DASH", ""] {
        let err = Profile::from_toml(&profile_with_acceptances(&[bad]))
            .expect_err("ill-formed acceptance name refuses");
        assert!(err.to_string().contains("must match"), "for {bad:?}");
    }
}

#[test]
fn declaration_secret_shaped_names_permitted() {
    // No deny on acceptances: exact names are operator intent.
    Profile::from_toml(&profile_with_acceptances(&["CISTELLA_ACC_TOKEN"]))
        .expect("secret-shaped acceptance declares");
}

#[test]
fn required_absent_fails_name_only() {
    let _guard = EnvGuard::remove(&["CISTELLA_ACC_MISSING"]);
    let prof = Profile::from_toml(&profile_with_acceptances(&["CISTELLA_ACC_MISSING"]))
        .expect("declaration parses without the var present");
    let err = prof
        .snapshot_acceptances()
        .expect_err("absent acceptance refuses");
    assert!(err.to_string().contains("CISTELLA_ACC_MISSING"));
}

#[test]
fn required_first_absent_in_list_order_reported() {
    let _guard = EnvGuard::remove(&["CISTELLA_ACC_ORD_B", "CISTELLA_ACC_ORD_A"]);
    let prof = Profile::from_toml(&profile_with_acceptances(&[
        "CISTELLA_ACC_ORD_B",
        "CISTELLA_ACC_ORD_A",
    ]))
    .unwrap();
    let err = prof.snapshot_acceptances().expect_err("absent refuses");
    assert!(
        err.to_string().contains("CISTELLA_ACC_ORD_B"),
        "first absent in list order reported"
    );
}

#[test]
fn collision_with_assignment_fails() {
    let _guard = EnvGuard::set(&[("CISTELLA_ACC_HIT", "v")]);
    // NOTE: the top-level list precedes the table — TOML keys after a
    // table header belong to that table.
    let toml = minimal_toml(
        "environment-acceptances = [\"CISTELLA_ACC_HIT\"]\n[environment-assignments]\nCISTELLA_ACC_HIT = \"assigned\"\n",
    );
    let prof = Profile::from_toml(&toml).unwrap();
    let err = prof.snapshot_acceptances().expect_err("overlap refuses");
    assert!(err.to_string().contains("CISTELLA_ACC_HIT"));
}

#[test]
fn collision_home_fails() {
    let prof = Profile::from_toml(&profile_with_acceptances(&["HOME"])).expect("declares");
    let err = prof.snapshot_acceptances().expect_err("HOME refuses");
    assert!(err.to_string().contains("HOME"));
}

#[test]
fn collision_ssh_auth_sock_conditional_on_surface() {
    let _guard = EnvGuard::set(&[("SSH_AUTH_SOCK", "/run/fake.sock")]);
    let agent = "image = \"localhost/cistella/opencode:example\"\ncredential-surface = { ssh_agent = \"/run/fake.sock\" }\nmounts = []\nenvironment-acceptances = [\"SSH_AUTH_SOCK\"]\n";
    let prof = Profile::from_toml(agent).unwrap();
    assert!(
        prof.snapshot_acceptances().is_err(),
        "injected name refuses under ssh_agent surface"
    );
    let none = Profile::from_toml(&profile_with_acceptances(&["SSH_AUTH_SOCK"])).unwrap();
    assert!(
        none.snapshot_acceptances().is_ok(),
        "same name forwards under none surface"
    );
}

#[test]
fn verbatim_template_looking_value_passes_through() {
    let _guard = EnvGuard::set(&[("CISTELLA_ACC_VERB", "{{supplement:x}}@literal")]);
    let prof = Profile::from_toml(&profile_with_acceptances(&["CISTELLA_ACC_VERB"])).unwrap();
    assert_eq!(
        prof.snapshot_acceptances().unwrap(),
        vec![(
            "CISTELLA_ACC_VERB".to_string(),
            "{{supplement:x}}@literal".to_string()
        )]
    );
}

#[test]
fn verbatim_equals_permitted() {
    let _guard = EnvGuard::set(&[("CISTELLA_ACC_EQ", "a=b=c")]);
    let prof = Profile::from_toml(&profile_with_acceptances(&["CISTELLA_ACC_EQ"])).unwrap();
    assert_eq!(
        prof.snapshot_acceptances().unwrap()[0].1,
        "a=b=c".to_string()
    );
}

#[test]
fn gate_line_breaks_refused_value_free() {
    for (tag, value) in [
        ("LF", "ZZZ_TOP_SECRET_LF_111\ntrailer"),
        ("CR", "ZZZ_TOP_SECRET_CR_222\rtrailer"),
    ] {
        let name = format!("CISTELLA_ACC_GATE_{tag}");
        let _guard = EnvGuard::set(&[(name.as_str(), value)]);
        let prof = Profile::from_toml(&profile_with_acceptances(&[name.as_str()])).unwrap();
        let err = prof.snapshot_acceptances().expect_err("line break refuses");
        let msg = err.to_string();
        assert!(msg.contains(&name), "names the variable");
        assert!(
            !msg.contains("ZZZ_TOP_SECRET"),
            "carries no part of the value"
        );
    }
}

#[test]
fn snapshot_non_unicode_value_refused_without_payload() {
    use std::os::unix::ffi::OsStringExt;
    let name = "CISTELLA_ACC_NONUNI";
    let prev = std::env::var_os(name);
    unsafe { std::env::set_var(name, std::ffi::OsString::from_vec(vec![0xFF, 0xFE, b'x'])) };
    let prof = Profile::from_toml(&profile_with_acceptances(&[name])).unwrap();
    let err = prof
        .snapshot_acceptances()
        .expect_err("non-Unicode refuses");
    match prev {
        Some(v) => unsafe { std::env::set_var(name, v) },
        None => unsafe { std::env::remove_var(name) },
    }
    // Exact match: the VarError payload (non-UTF8 bytes, unprintable by
    // construction) contributes nothing to the diagnostic.
    assert_eq!(
        err.to_string(),
        format!("profile: environment-acceptances name absent or non-Unicode: {name}")
    );
}

#[test]
fn secret_shaped_name_forwards_through_snapshot() {
    // No-deny pinned on the forwarding path, not just declaration parsing.
    let _guard = EnvGuard::set(&[("CISTELLA_ACC_LIVE_TOKEN", "token-value-7")]);
    let prof = Profile::from_toml(&profile_with_acceptances(&["CISTELLA_ACC_LIVE_TOKEN"])).unwrap();
    assert_eq!(
        prof.snapshot_acceptances().unwrap(),
        vec![(
            "CISTELLA_ACC_LIVE_TOKEN".to_string(),
            "token-value-7".to_string()
        )]
    );
}

#[test]
fn no_render_permission_for_accepted_names() {
    let _guard = EnvGuard::set(&[("CISTELLA_ACC_NR", "v")]);
    let toml = minimal_toml(
        "environment-acceptances = [\"CISTELLA_ACC_NR\"]\n[environment-assignments]\nPROBE = \"{{environment:CISTELLA_ACC_NR}}\"\n",
    );
    let err = resolve_text(&toml).expect_err("accepted name has no render rights");
    assert!(
        err.to_string().contains("CISTELLA_ACC_NR"),
        "allowlist-absent diagnostic names the span"
    );
}

#[test]
fn shared_gate_parity_with_assignments() {
    // Assignments reject line breaks at validation and permit `=`.
    Profile::from_toml(&minimal_toml(
        "[environment-assignments]\nEQ_OK = \"a=b\"\n",
    ))
    .expect("assignment permits =");
    // Escaped TOML values deserialize to real line breaks, so these reach
    // the shared value gate (a literal line break would fail TOML parsing
    // first — the false positive this guards against).
    for (tag, esc) in [("LF", "\\n"), ("CR", "\\r")] {
        let toml = minimal_toml(&format!(
            "[environment-assignments]\nBAD_{tag} = \"a{esc}b\"\n"
        ));
        let err = Profile::from_toml(&toml).expect_err("assignment line break refuses");
        let msg = err.to_string();
        assert!(
            msg.contains("must not contain newlines"),
            "gate error, not parse error: {msg}"
        );
        assert!(!msg.contains("TOML parse"), "reached validation: {msg}");
    }
    // Assignment NUL reaches the render gate: the unit refuses it.
    let session = cistella::session::Session {
        id: mint_session_id(),
        directory: "/tmp".to_string(),
        profile: "p".to_string(),
        profile_digest: "d".to_string(),
        identity: "i".to_string(),
        command: vec!["true".to_string()],
        image: "img".to_string(),
        container_home: "/home/cistella".to_string(),
    };
    assert!(
        cistella::runtime::generate_quadlet_unit(&session, &[], &["NUL_BAD=a\0b".to_string()], &[])
            .is_err(),
        "render gate refuses NUL in env"
    );
}

#[test]
fn accepted_value_round_trips_post_escaping() {
    // Semantic equality in-container post-escaping, not unit syntax:
    // render the unit, read the Environment= line back through unquoting.
    let tricky = "sq'q dq\"q bs\\q 100% ok";
    let _guard = EnvGuard::set(&[("CISTELLA_ACC_RT", tricky)]);
    let prof = Profile::from_toml(&profile_with_acceptances(&["CISTELLA_ACC_RT"])).unwrap();
    let pairs = prof.snapshot_acceptances().unwrap();
    let session = cistella::session::Session {
        id: mint_session_id(),
        directory: "/tmp".to_string(),
        profile: "p".to_string(),
        profile_digest: "d".to_string(),
        identity: "i".to_string(),
        command: vec!["true".to_string()],
        image: "img".to_string(),
        container_home: "/home/cistella".to_string(),
    };
    let env_extra = pairs
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>();
    let unit = cistella::runtime::generate_quadlet_unit(&session, &[], &env_extra, &[]).unwrap();
    let line = unit
        .lines()
        .find(|l| l.starts_with("Environment=CISTELLA_ACC_RT="))
        .expect("accepted pair renders");
    let rendered = line.strip_prefix("Environment=CISTELLA_ACC_RT=").unwrap();
    assert_eq!(cistella::runtime::unquote_systemd(rendered), tricky);
}
