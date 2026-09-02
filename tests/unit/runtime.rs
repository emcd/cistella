//! Quadlet generation.

use cistella::runtime::{SessionId, generate_quadlet_unit};

#[test]
fn quadlet_uses_tmpfs_key() {
    let sess = SessionId {
        session_id: "s1".to_string(),
        seat: "alice".to_string(),
        harness: "opencode".to_string(),
        profile: "default".to_string(),
    };
    let volumes = vec![
        "--tmpfs".to_string(),
        "/home/cistella".to_string(),
        "--volume".to_string(),
        "/tmp/a:/work:rw".to_string(),
    ];
    let unit = generate_quadlet_unit(
        &sess,
        "cistella/opencode:example",
        &volumes,
        &[],
        "/home/cistella",
    )
    .unwrap();
    assert!(
        unit.contains("Tmpfs=/home/cistella"),
        "expected Tmpfs=, got {unit}"
    );
    assert!(
        !unit.contains("Volume=/home/cistella:tmpfs"),
        "wrong Volume tmpfs syntax"
    );
    assert!(unit.contains("Image=cistella/opencode:example"));
    assert!(unit.contains("UserNS=keep-id"));
    assert!(unit.contains("Environment=HOME=/home/cistella"));
    // Closed env not baked
    assert!(!unit.contains("Environment=TERM="));
}

#[test]
fn quadlet_labels_present() {
    let sess = SessionId {
        session_id: "abc".to_string(),
        seat: "bob".to_string(),
        harness: "opencode".to_string(),
        profile: "p1".to_string(),
    };
    let unit = generate_quadlet_unit(&sess, "img", &[], &[], "/home/cistella").unwrap();
    assert!(unit.contains("Label=cistella.session-id=abc"));
    assert!(unit.contains("Label=cistella.seat=bob"));
}

#[test]
fn quadlet_rejects_injection() {
    let sess = SessionId {
        session_id: "a\n[Service]\nExec=bad".to_string(),
        seat: "bob".to_string(),
        harness: "opencode".to_string(),
        profile: "p1".to_string(),
    };
    assert!(generate_quadlet_unit(&sess, "img", &[], &[], "/home/cistella").is_err());
}
