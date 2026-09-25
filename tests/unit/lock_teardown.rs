//! Lock-held converge pin: the prepare-failure path runs its
//! teardown while holding the creation-window guard, so only the
//! lock-held half (`teardown_inner`) may run there — the full
//! `teardown` re-acquires the guard and deadlocks nested (lock.rs
//! documents that guards must never nest). This test holds the
//! guard and runs the inner half with a join timeout: a
//! reintroduced full-teardown call wedges instead of completing,
//! failing loudly rather than hanging the suite silently.

#[test]
fn lock_held_inner_teardown_completes() {
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _guard = cistella::lock::LockGuard::acquire().expect("lock acquires");
        let _ = cistella::runtime::teardown_inner("no-such-container", "no-such-session");
        let _ = done_tx.send(());
    });
    done_rx
        .recv_timeout(std::time::Duration::from_secs(15))
        .expect("lock-held converge must complete, not wedge");
}
