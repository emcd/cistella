//! Quadlet generation, minting, labels, and selectors.

use cistella::registry::{SessionRecord, resolve_exact};
use cistella::runtime::{generate_quadlet_unit, quote_systemd, unquote_systemd};
use cistella::session::{
    MINTED_ID_LEN, Session, command_label, mint_session_id, parse_cli_label, parse_command_label,
    validate_generic_label,
};

fn test_session() -> Session {
    Session {
        id: mint_session_id(),
        directory: "/tmp/work".to_string(),
        profile: "default".to_string(),
        profile_digest: "abc123".to_string(),
        identity: "alice".to_string(),
        command: vec!["sleep".to_string(), "infinity".to_string()],
        image: "localhost/cistella/opencode:example".to_string(),
        container_home: "/home/cistella".to_string(),
    }
}

fn test_record(id: &str, directory: &str) -> SessionRecord {
    SessionRecord {
        id: id.to_string(),
        container_name: format!("cistella-{id}"),
        directory: directory.to_string(),
        profile: "default".to_string(),
        identity: "alice".to_string(),
        image: "img".to_string(),
        active_state: "active".to_string(),
        container_present: true,
        generic_labels: vec![("agentmux.session".to_string(), "s1".to_string())],
    }
}

#[test]
fn quadlet_uses_tmpfs_key() {
    let sess = test_session();
    let volumes = vec![
        "--tmpfs".to_string(),
        "/home/cistella".to_string(),
        "--volume".to_string(),
        "/tmp/a:/work:rw".to_string(),
    ];
    let unit = generate_quadlet_unit(&sess, &volumes, &[], &[]).unwrap();
    assert!(
        unit.contains("Tmpfs=/home/cistella"),
        "Tmpfs stays raw (Quadlet quotes it for ExecStart itself), got {unit}"
    );
    assert!(
        !unit.contains("Volume=/home/cistella:tmpfs"),
        "wrong Volume tmpfs syntax"
    );
    assert!(unit.contains("Image=localhost/cistella/opencode:example"));
    assert!(unit.contains("UserNS=keep-id"));
    assert!(unit.contains("Environment=HOME=\"/home/cistella\""));
    // Closed env not baked
    assert!(!unit.contains("Environment=TERM="));
}

#[test]
fn quadlet_runs_container_under_init() {
    let sess = test_session();
    let unit = generate_quadlet_unit(&sess, &[], &[], &[]).unwrap();
    // Scoped to the [Container] section: tini as PID 1 forwards SIGTERM
    // to `sleep infinity` (bare PID 1 ignores it, stalling stop for the
    // full StopTimeout) and reaps zombies.
    let container = unit
        .split("[Container]")
        .nth(1)
        .expect("Container section")
        .split("[Service]")
        .next()
        .expect("Service section follows");
    assert!(
        container.lines().any(|l| l == "RunInit=true"),
        "RunInit=true missing from [Container], got {unit}"
    );
}

#[test]
fn quadlet_labels_present() {
    let sess = test_session();
    let unit = generate_quadlet_unit(&sess, &[], &[], &[]).unwrap();
    assert!(unit.contains(&format!("Label=cistella.id=\"{}\"", sess.id)));
    assert!(unit.contains("Label=cistella.directory=\"/tmp/work\""));
    assert!(unit.contains("Label=cistella.profile=\"default\""));
    assert!(unit.contains("Label=cistella.identity=\"alice\""));
    assert!(unit.contains("Label=cistella.command=\"[\\\"sleep\\\",\\\"infinity\\\"]\""));
    assert!(unit.contains("Label=cistella.image="));
    assert!(unit.contains(&format!("ContainerName=cistella-{}", sess.id)));
}

#[test]
fn quadlet_driver_labels_last() {
    let sess = test_session();
    let generic = vec![("agentmux.session".to_string(), "s1".to_string())];
    let unit = generate_quadlet_unit(&sess, &[], &[], &generic).unwrap();
    let generic_pos = unit.find("Label=agentmux.session=").expect("generic label");
    let driver_pos = unit.find("Label=cistella.id=").expect("driver label");
    assert!(generic_pos < driver_pos, "driver-owned labels render last");
}

#[test]
fn systemd_quote_round_trip() {
    for value in [
        "plain",
        "/tmp/my dir",
        "a=b c\"d'e",
        "back\\slash",
        "[\"sleep\",\"infinity\"]",
        "echo \"x=y z\"; sleep 300",
        "100% %h %%",
        "%o %t %N",
    ] {
        assert_eq!(
            unquote_systemd(&quote_systemd(value)),
            value,
            "round-trip {value:?}"
        );
    }
    // Unquoted legacy values pass through untouched.
    assert_eq!(unquote_systemd("abc123"), "abc123");
}

#[test]
fn systemd_percent_escapes() {
    assert_eq!(quote_systemd("100% %h"), "\"100%% %%h\"");
    assert_eq!(unquote_systemd("\"100%% %%h\""), "100% %h");
    assert_eq!(
        cistella::runtime::escape_percent("/tmp/pct100%"),
        "/tmp/pct100%%"
    );
}

#[test]
fn quadlet_command_label_survives_quoting() {
    // The registry reads the unit file with the same unquoting Quadlet
    // applies, so the JSON must parse back to the exact argv.
    let mut sess = test_session();
    sess.command = vec![
        "sh".to_string(),
        "-c".to_string(),
        "echo \"a=b c'd\" > /tmp/edge_probe; sleep 60".to_string(),
    ];
    let unit = generate_quadlet_unit(&sess, &[], &[], &[]).unwrap();
    let line = unit
        .lines()
        .find(|l| l.starts_with("Label=cistella.command="))
        .expect("command label line");
    let stored = unquote_systemd(line.strip_prefix("Label=cistella.command=").unwrap());
    assert_eq!(parse_command_label(&stored).unwrap(), sess.command);
}

#[test]
fn quadlet_rejects_injection() {
    let mut sess = test_session();
    sess.identity = "a\n[Service]\nExec=bad".to_string();
    assert!(generate_quadlet_unit(&sess, &[], &[], &[]).is_err());
}

#[test]
fn quadlet_rejects_reserved_generic_label() {
    let sess = test_session();
    let generic = vec![("cistella.id".to_string(), "spoof".to_string())];
    assert!(generate_quadlet_unit(&sess, &[], &[], &generic).is_err());
}

#[test]
fn mint_is_fixed_lowercase_time_sortable() {
    let first = mint_session_id();
    assert_eq!(first.len(), MINTED_ID_LEN, "minted id: {first}");
    assert!(
        first
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()),
        "minted id charset: {first}"
    );
    std::thread::sleep(std::time::Duration::from_millis(3));
    let second = mint_session_id();
    assert!(second > first, "{second} should sort after {first}");
    assert_ne!(first, second);
}

#[test]
fn command_json_round_trip_lossless() {
    let argv = vec![
        "opencode".to_string(),
        "--model".to_string(),
        "x y=z\"q".to_string(),
    ];
    let label = command_label(&argv);
    assert_eq!(parse_command_label(&label).unwrap(), argv);
}

#[test]
fn generic_label_rules() {
    assert!(validate_generic_label("agentmux.session", "s1").is_ok());
    assert!(validate_generic_label("cistella.id", "spoof").is_err());
    assert!(validate_generic_label("agentmux.session", "a=b").is_err());
    assert!(validate_generic_label("agentmux.session", "a\nb").is_err());
    assert!(parse_cli_label("k=v").is_ok());
    assert!(parse_cli_label("novalue").is_err());
    assert!(parse_cli_label("cistella.id=spoof").is_err());
}

#[test]
fn remove_scratch_missing_ok_blocker_errs() {
    // Missing scratch paths are Ok; a path blocked by a non-directory
    // fails loudly instead of reporting success with residue.
    let id = format!("zz9nosuch{}", std::process::id());
    let blocker = std::path::PathBuf::from(format!("/tmp/cistella-{id}"));
    let _ = std::fs::remove_file(&blocker);
    assert!(cistella::runtime::remove_scratch(&id).is_ok());
    std::fs::write(&blocker, "block").unwrap();
    assert!(cistella::runtime::remove_scratch(&id).is_err());
    std::fs::remove_file(&blocker).unwrap();
}

#[test]
fn residue_gone_for_paths() {
    let dir = tempfile::tempdir().unwrap();
    let unit = dir.path().join("cistella-abc.container");
    let scratch = dir.path().join("scratch");
    // Nothing exists: gone.
    assert!(cistella::runtime::residue_gone_for_paths(
        &unit,
        std::slice::from_ref(&scratch)
    ));
    // Unit file present: residue.
    std::fs::write(&unit, "x").unwrap();
    assert!(!cistella::runtime::residue_gone_for_paths(
        &unit,
        std::slice::from_ref(&scratch)
    ));
    std::fs::remove_file(&unit).unwrap();
    // Scratch present: residue.
    std::fs::create_dir(&scratch).unwrap();
    assert!(!cistella::runtime::residue_gone_for_paths(
        &unit,
        std::slice::from_ref(&scratch)
    ));
}

#[test]
fn resolve_exact_single_prefix() {
    let records = vec![
        test_record("aaa111", "/tmp/a"),
        test_record("bbb222", "/tmp/b"),
    ];
    let found = resolve_exact(&records, Some("aaa"), None, &[]).unwrap();
    assert_eq!(found.id, "aaa111");
}

#[test]
fn resolve_exact_rejects_mixed_and_ambiguous() {
    let records = vec![
        test_record("aaa111", "/tmp/a"),
        test_record("aaa222", "/tmp/a"),
    ];
    // Mixed forms are usage error.
    assert!(resolve_exact(&records, Some("aaa"), Some("/tmp/a"), &[]).is_err());
    // Ambiguous prefix lists candidates.
    let err = resolve_exact(&records, Some("aaa"), None, &[]).unwrap_err();
    assert!(err.to_string().contains("ambiguous"));
    // Empty match names the registry.
    let err = resolve_exact(&records, Some("zzz"), None, &[]).unwrap_err();
    assert!(err.to_string().contains("no session"));
    // Directory selector matches both -> ambiguous.
    let err = resolve_exact(&records, None, Some("/tmp/a"), &[]).unwrap_err();
    assert!(err.to_string().contains("ambiguous"));
    // Label selector ANDs across entries.
    let labels = vec![("agentmux.session".to_string(), "s1".to_string())];
    assert!(resolve_exact(&records, None, None, &labels).is_err());
    let solo = vec![test_record("aaa111", "/tmp/a")];
    assert!(resolve_exact(&solo, None, None, &labels).is_ok());
}

#[test]
fn harness_exec_pins_workdir_before_container() {
    let args = cistella::transport::exec_harness_args(
        "cistella-abc",
        "/home/me/src/cistella",
        &["true".to_string()],
    );
    let workdir = args
        .iter()
        .position(|a| a == "--workdir")
        .expect("workdir flag");
    assert_eq!(args[workdir + 1], "/home/me/src/cistella");
    assert_eq!(args[workdir + 2], "cistella-abc");
    assert!(args.contains(&"-i".to_string()));
    assert!(args.contains(&"-t".to_string()));
}
