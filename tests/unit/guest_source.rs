//! Registered-guest sources: path verification and bounded fetch.
//!
//! Path tests pin canonicalization, ancestor, symlink, ownership,
//! and digest refusals; fetch tests run against a fixture HTTP
//! origin on 127.0.0.1 (no external network — fixture bytes only).

use cistella::guest_registry::HostEnv;
use std::os::unix::fs::PermissionsExt;

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
    use cistella::guest_source::resolve_path_source;
    let dir = tempfile::tempdir().expect("tmpdir");
    let (path, bytes) = write_guest(&dir, "guest", 0o755);
    let pinned = resolve_path_source(&path, &fixture_digest(&bytes)).expect("valid resolves");
    assert_eq!(pinned.digest, fixture_digest(&bytes));
}

#[test]
fn replaced_path_guest_refuses_at_conduct() {
    use cistella::guest_source::resolve_path_source;
    let dir = tempfile::tempdir().expect("tmpdir");
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
    use cistella::guest_source::resolve_path_source;
    let error = resolve_path_source(
        std::path::Path::new("relative/guest"),
        &fixture_digest(b"x"),
    )
    .expect_err("relative must refuse");
    assert!(error.to_string().contains("absolute"), "got: {error}");
}

#[test]
fn symlink_final_registered_path_refuses() {
    use cistella::guest_source::resolve_path_source;
    let dir = tempfile::tempdir().expect("tmpdir");
    let (real, bytes) = write_guest(&dir, "real", 0o755);
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&real, &link).expect("symlink");
    let error =
        resolve_path_source(&link, &fixture_digest(&bytes)).expect_err("symlink must refuse");
    assert!(error.to_string().contains("symlink"), "got: {error}");
}

#[test]
fn non_executable_registered_path_refuses() {
    use cistella::guest_source::resolve_path_source;
    let dir = tempfile::tempdir().expect("tmpdir");
    let (path, bytes) = write_guest(&dir, "guest", 0o644);
    let error =
        resolve_path_source(&path, &fixture_digest(&bytes)).expect_err("non-exec must refuse");
    assert!(error.to_string().contains("not executable"), "got: {error}");
}

#[test]
fn group_writable_registered_path_refuses() {
    use cistella::guest_source::resolve_path_source;
    let dir = tempfile::tempdir().expect("tmpdir");
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
    use cistella::guest_source::resolve_path_source;
    let dir = tempfile::tempdir().expect("tmpdir");
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
    use cistella::guest_source::resolve_path_source;
    let dir = tempfile::tempdir().expect("tmpdir");
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
        // Restore owner-write access for tmpdir cleanup on strict umasks.
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
}

#[test]
fn cache_dir_location_rules() {
    use cistella::guest_source::cache_dir;
    // XDG set: direct.
    let env = HostEnv::new(None, Some("/seat/cache"), Some("/home/seat"));
    assert_eq!(
        cache_dir(&env).expect("xdg cache path"),
        std::path::PathBuf::from("/seat/cache/cistella/guest-cache")
    );
    // XDG unset or empty: HOME fallback.
    for xdg in [None, Some("")] {
        let env = HostEnv::new(None, xdg, Some("/home/seat"));
        assert_eq!(
            cache_dir(&env).expect("fallback cache path"),
            std::path::PathBuf::from("/home/seat/.cache/cistella/guest-cache")
        );
    }
    // Relative XDG refuses; missing HOME refuses.
    let env = HostEnv::new(None, Some("cache"), Some("/home/seat"));
    let error = cache_dir(&env).expect_err("relative XDG cache must refuse");
    assert!(error.to_string().contains("absolute"), "got: {error}");
    let env = HostEnv::new(None, None, None);
    let error = cache_dir(&env).expect_err("missing HOME must refuse");
    assert!(error.to_string().contains("HOME"), "got: {error}");
}

#[test]
fn missing_registered_path_refuses_typed() {
    use cistella::guest_source::resolve_path_source;
    let dir = tempfile::tempdir().expect("tmpdir");
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

/// One-shot fixture HTTP origin: serves queued responses (status,
/// body, per-connection delay) on 127.0.0.1, counting hits. No
/// external network — fixture bytes only. The server thread is
/// detached (each test binds its own ephemeral port; the process
/// exit reaps stragglers) and exits after its queue drains or a
/// budget passes, so a failing test can never hang the suite.
struct FixtureOrigin {
    url: String,
    hits: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

fn serve_queued(responses: Vec<(u16, Vec<u8>, std::time::Duration)>) -> FixtureOrigin {
    use std::io::{Read, Write};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind fixture origin");
    let url = format!(
        "http://{}/guest",
        listener.local_addr().expect("local addr")
    );
    let hits = std::sync::Arc::new(AtomicUsize::new(0));
    let counter = std::sync::Arc::clone(&hits);
    listener.set_nonblocking(true).expect("nonblocking");
    std::thread::spawn(move || {
        let mut queue: std::collections::VecDeque<_> = responses.into_iter().collect();
        let start = std::time::Instant::now();
        while start.elapsed() < std::time::Duration::from_secs(15) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    counter.fetch_add(1, Ordering::SeqCst);
                    let mut head = Vec::new();
                    let mut byte = [0u8; 1];
                    while !head.ends_with(b"\r\n\r\n") && head.len() < 8192 {
                        match stream.read(&mut byte) {
                            Ok(0) | Err(_) => break,
                            Ok(_) => head.extend_from_slice(&byte),
                        }
                    }
                    let Some((status, body, delay)) = queue.pop_front() else {
                        break;
                    };
                    if !delay.is_zero() {
                        std::thread::sleep(delay);
                    }
                    let reason = match status {
                        200 => "OK",
                        404 => "Not Found",
                        _ => "Error",
                    };
                    let header = format!(
                        "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(header.as_bytes());
                    let _ = stream.write_all(&body);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if queue.is_empty() {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }
    });
    FixtureOrigin { url, hits }
}

fn fetch_entry(url: &str, digest: [u8; 32]) -> cistella::guest_registry::Guest {
    cistella::guest_registry::Guest {
        name: "fetched".to_string(),
        role: "fetched-role".to_string(),
        protocol: 2,
        source: cistella::guest_registry::SourceLocation::Fetch {
            source_url: url.to_string(),
            digest,
        },
        arguments: Vec::new(),
    }
}

fn fetch_ctx(dir: &tempfile::TempDir) -> cistella::guest_source::ResolveCtx {
    cistella::guest_source::ResolveCtx::new(dir.path().join("cache"))
}

#[test]
fn ancestor_verdict_truth_table() {
    use cistella::guest_source::{AncestorVerdict, ancestor_verdict};
    let seat = 1000u32;
    // Not world-writable: safe for any owner.
    assert_eq!(ancestor_verdict(0o755, 9999, seat), AncestorVerdict::Pass);
    assert_eq!(ancestor_verdict(0o755, seat, seat), AncestorVerdict::Pass);
    // Non-sticky world-writable: any uid swaps entries.
    assert_eq!(
        ancestor_verdict(0o777, seat, seat),
        AncestorVerdict::WorldWritable
    );
    assert_eq!(
        ancestor_verdict(0o777, 0, seat),
        AncestorVerdict::WorldWritable
    );
    assert_eq!(
        ancestor_verdict(0o777, 9999, seat),
        AncestorVerdict::WorldWritable
    );
    // Sticky world-writable: only the directory owner swaps
    // entries, so the owner must be trusted.
    assert_eq!(ancestor_verdict(0o1777, seat, seat), AncestorVerdict::Pass);
    assert_eq!(ancestor_verdict(0o1777, 0, seat), AncestorVerdict::Pass);
    assert_eq!(
        ancestor_verdict(0o1777, 9999, seat),
        AncestorVerdict::StickyUntrusted
    );
}

#[test]
fn sticky_untrusted_owner_ancestor_refuses() {
    // Needs privilege (chown a directory to another uid); without
    // it the arm is review-covered plus truth-table-pinned above.
    // SAFETY: getuid is inherently safe.
    if unsafe { nix::libc::getuid() } != 0 {
        eprintln!("skip: sticky-untrusted-owner needs root (chown)");
        return;
    }
    use cistella::guest_source::resolve_path_source;
    let dir = tempfile::tempdir().expect("tmpdir");
    let sub = dir.path().join("sticky-alien");
    std::fs::create_dir(&sub).expect("mkdir");
    let bytes = b"#!/bin/sh\nexit 0\n".to_vec();
    std::fs::write(sub.join("guest"), &bytes).expect("write fixture");
    std::fs::set_permissions(sub.join("guest"), std::fs::Permissions::from_mode(0o755))
        .expect("chmod");
    std::os::unix::fs::chown(&sub, Some(65534), None).expect("chown nobody");
    std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o1777)).expect("chmod");
    let error = resolve_path_source(&sub.join("guest"), &fixture_digest(&bytes))
        .expect_err("alien sticky must refuse");
    assert!(
        error.to_string().contains("sticky but not seat-owned"),
        "got: {error}"
    );
    std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

#[test]
fn dangling_symlink_slot_refuses_without_fetch() {
    use cistella::guest_source::{ResolveCtx, fetch_source};
    // A dangling symlink at the hash slot must refuse as a
    // poisoned slot (typed, zero HTTP requests, symlink left in
    // place) — never read as a miss that fetches over it.
    let dir = tempfile::tempdir().expect("tmpdir");
    let bytes = b"fixture bytes".to_vec();
    let digest = fixture_digest(&bytes);
    let origin = serve_queued(vec![(200, bytes, std::time::Duration::ZERO)]);
    let ctx = ResolveCtx::new(dir.path().join("cache"));
    std::fs::create_dir_all(&ctx.cache_dir).expect("mkdir cache");
    let slot = ctx.cache_dir.join(hex::encode(digest));
    std::os::unix::fs::symlink(dir.path().join("dangling-target"), &slot).expect("symlink slot");
    let error = fetch_source(&origin.url, &digest, &ctx).expect_err("dangling slot must refuse");
    assert!(error.to_string().contains("symlink"), "got: {error}");
    assert_eq!(
        origin.hits.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "refusal precedes any HTTP request"
    );
    assert!(
        std::fs::symlink_metadata(&slot)
            .expect("slot metadata")
            .is_symlink(),
        "symlink left in place, never overwritten"
    );
}

#[test]
fn cache_dir_symlink_refuses() {
    use cistella::guest_source::{ResolveCtx, fetch_source};
    let dir = tempfile::tempdir().expect("tmpdir");
    let real = dir.path().join("real-cache");
    std::fs::create_dir(&real).expect("mkdir");
    let link = dir.path().join("link-cache");
    std::os::unix::fs::symlink(&real, &link).expect("symlink cache");
    let bytes = b"fixture bytes".to_vec();
    let origin = serve_queued(vec![(200, bytes.clone(), std::time::Duration::ZERO)]);
    let ctx = ResolveCtx::new(link);
    let error = fetch_source(&origin.url, &fixture_digest(&bytes), &ctx)
        .expect_err("symlinked cache must refuse");
    assert!(error.to_string().contains("symlink"), "got: {error}");
    assert_eq!(
        origin.hits.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "refusal precedes any HTTP request"
    );
}

#[test]
fn cache_dir_foreign_owner_refuses() {
    // Needs privilege (chown a directory to another uid); without
    // it the arm is review-covered (same predicate as the file
    // ownership gate).
    // SAFETY: getuid is inherently safe.
    if unsafe { nix::libc::getuid() } != 0 {
        eprintln!("skip: foreign-owner cache needs root (chown)");
        return;
    }
    use cistella::guest_source::{ResolveCtx, fetch_source};
    let dir = tempfile::tempdir().expect("tmpdir");
    let cache = dir.path().join("cache");
    std::fs::create_dir(&cache).expect("mkdir");
    std::os::unix::fs::chown(&cache, Some(65534), None).expect("chown nobody");
    let bytes = b"fixture bytes".to_vec();
    let origin = serve_queued(vec![(200, bytes.clone(), std::time::Duration::ZERO)]);
    let ctx = ResolveCtx::new(cache);
    let error = fetch_source(&origin.url, &fixture_digest(&bytes), &ctx)
        .expect_err("foreign cache must refuse");
    assert!(
        error.to_string().contains("seat- or root-owned"),
        "got: {error}"
    );
    assert_eq!(
        origin.hits.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "refusal precedes any HTTP request"
    );
}

#[test]
fn fetch_miss_slots_and_hit_reverifies_without_refetch() {
    use cistella::guest_source::resolve;
    let dir = tempfile::tempdir().expect("tmpdir");
    let bytes = b"#!/bin/sh\nexit 0\n".to_vec();
    let origin = serve_queued(vec![(200, bytes.clone(), std::time::Duration::ZERO)]);
    let ctx = fetch_ctx(&dir);
    let entry = fetch_entry(&origin.url, fixture_digest(&bytes));
    let first = resolve(&entry, &ctx).expect("miss downloads");
    assert_eq!(first.pinned.digest, fixture_digest(&bytes));
    assert_eq!(
        first.display,
        ctx.cache_dir.join(hex::encode(fixture_digest(&bytes)))
    );
    let second = resolve(&entry, &ctx).expect("hit re-verifies");
    assert_eq!(second.pinned.digest, fixture_digest(&bytes));
    assert_eq!(
        origin.hits.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "cache hit must not refetch"
    );
}

#[test]
fn fetch_digest_mismatch_refuses_pre_create_leaving_no_slot() {
    use cistella::guest_source::resolve;
    let dir = tempfile::tempdir().expect("tmpdir");
    let origin = serve_queued(vec![(
        200,
        b"attacker bytes".to_vec(),
        std::time::Duration::ZERO,
    )]);
    let ctx = fetch_ctx(&dir);
    let entry = fetch_entry(&origin.url, fixture_digest(b"expected bytes"));
    let error = resolve(&entry, &ctx).expect_err("mismatch must refuse");
    assert!(
        error.to_string().contains("digest mismatch"),
        "got: {error}"
    );
    assert!(
        !ctx.cache_dir
            .join(hex::encode(fixture_digest(b"expected bytes")))
            .exists(),
        "no partial cache entry may remain"
    );
}

#[test]
fn fetch_rotation_requires_registry_edit() {
    use cistella::guest_source::resolve;
    let dir = tempfile::tempdir().expect("tmpdir");
    let old_bytes = b"v1 bytes".to_vec();
    let new_bytes = b"v2 bytes".to_vec();
    let origin = serve_queued(vec![
        (200, old_bytes.clone(), std::time::Duration::ZERO),
        (200, new_bytes.clone(), std::time::Duration::ZERO),
    ]);
    let ctx = fetch_ctx(&dir);
    // Same URL, old hash: pins the old slot.
    let old = fetch_entry(&origin.url, fixture_digest(&old_bytes));
    resolve(&old, &ctx).expect("old slot fetches");
    // Same URL, new hash (the registry edit): fetches the new slot.
    let new = fetch_entry(&origin.url, fixture_digest(&new_bytes));
    let resolved = resolve(&new, &ctx).expect("new slot fetches");
    assert_eq!(resolved.pinned.digest, fixture_digest(&new_bytes));
    // The old entry keeps its pinned slot with no new fetch.
    resolve(&old, &ctx).expect("old slot hits");
    assert_eq!(
        origin.hits.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "rotation fetches once per hash, never silently"
    );
}

#[test]
fn poisoned_cache_slot_refuses_per_use() {
    use cistella::guest_source::resolve;
    let dir = tempfile::tempdir().expect("tmpdir");
    let bytes = b"#!/bin/sh\nexit 0\n".to_vec();
    let origin = serve_queued(vec![(200, bytes.clone(), std::time::Duration::ZERO)]);
    let ctx = fetch_ctx(&dir);
    let entry = fetch_entry(&origin.url, fixture_digest(&bytes));
    resolve(&entry, &ctx).expect("pristine slot resolves");
    let slot = ctx.cache_dir.join(hex::encode(fixture_digest(&bytes)));
    std::fs::write(&slot, b"poisoned").expect("poison slot");
    let error = resolve(&entry, &ctx).expect_err("poisoned slot must refuse");
    assert!(
        error.to_string().contains("digest mismatch"),
        "got: {error}"
    );
    assert_eq!(
        origin.hits.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "poison refuses, never refetches"
    );
}

#[test]
fn fetch_error_status_and_size_bound_refuse() {
    use cistella::guest_source::resolve;
    let dir = tempfile::tempdir().expect("tmpdir");
    let origin = serve_queued(vec![(404, b"nope".to_vec(), std::time::Duration::ZERO)]);
    let ctx = fetch_ctx(&dir);
    let entry = fetch_entry(&origin.url, fixture_digest(b"nope"));
    let error = resolve(&entry, &ctx).expect_err("error status must refuse");
    assert!(error.to_string().contains("status 404"), "got: {error}");
    // Oversize body against a tiny bound: refuses, slots nothing.
    let big = vec![0x41u8; 2048];
    let origin = serve_queued(vec![(200, big.clone(), std::time::Duration::ZERO)]);
    let mut ctx = fetch_ctx(&dir);
    ctx.fetch_max_bytes = 16;
    let entry = fetch_entry(&origin.url, fixture_digest(&big));
    let error = resolve(&entry, &ctx).expect_err("oversize must refuse");
    assert!(error.to_string().contains("size bound"), "got: {error}");
    assert!(
        !ctx.cache_dir
            .join(hex::encode(fixture_digest(&big)))
            .exists(),
        "oversize fetch slots nothing"
    );
}

#[test]
fn fetch_deadline_refuses() {
    use cistella::guest_source::resolve;
    let dir = tempfile::tempdir().expect("tmpdir");
    let bytes = b"slow bytes".to_vec();
    let origin = serve_queued(vec![(
        200,
        bytes.clone(),
        std::time::Duration::from_secs(5),
    )]);
    let mut ctx = fetch_ctx(&dir);
    ctx.fetch_timeout = std::time::Duration::from_millis(300);
    let entry = fetch_entry(&origin.url, fixture_digest(&bytes));
    let error = resolve(&entry, &ctx).expect_err("deadline must refuse");
    assert!(error.to_string().contains("registry fetch"), "got: {error}");
}

#[test]
fn wrong_protocol_major_refuses_at_resolve() {
    use cistella::guest_registry::{Guest, SourceLocation};
    use cistella::guest_source::resolve;
    let dir = tempfile::tempdir().expect("tmpdir");
    let entry = Guest {
        name: "future".to_string(),
        role: "future-role".to_string(),
        protocol: 999,
        source: SourceLocation::Path {
            source_path: dir.path().join("guest"),
            digest: fixture_digest(b"x"),
        },
        arguments: Vec::new(),
    };
    let error = resolve(&entry, &fetch_ctx(&dir)).expect_err("future major must refuse");
    assert!(error.to_string().contains("protocol"), "got: {error}");
}

#[test]
fn substitution_after_guard_open_stages_into_guarded_dir() {
    use cistella::guest_source::prepare_cache_dir;
    // The exact scenario the path-based checks missed: the cache
    // path is substituted AFTER the guard opens (real dir renamed
    // aside, attacker symlink planted at the path). Every stage
    // and slot operation runs relative to the held directory FD,
    // so the fetch lands in the guarded original — never through
    // the swapped path.
    let dir = tempfile::tempdir().expect("tmpdir");
    let cache = dir.path().join("cache");
    let guard = prepare_cache_dir(&cache).expect("guard opens");
    let real = dir.path().join("cache-real");
    std::fs::rename(&cache, &real).expect("rename real aside");
    let attacker = dir.path().join("attacker");
    std::fs::create_dir(&attacker).expect("mkdir attacker");
    std::os::unix::fs::symlink(&attacker, &cache).expect("plant symlink");
    let bytes = b"guarded bytes".to_vec();
    let digest = fixture_digest(&bytes);
    let origin = serve_queued(vec![(200, bytes.clone(), std::time::Duration::ZERO)]);
    let outcome = guard
        .fetch(
            &origin.url,
            &digest,
            1024 * 1024,
            std::time::Duration::from_secs(10),
        )
        .expect("fetch stages into guarded dir");
    assert_eq!(outcome.pinned.digest, digest);
    // The slot names the guarded original (display path joins the
    // guard's path, which still names the renamed-aside dir only
    // by string — the bytes live in the real dir).
    assert_eq!(outcome.slot, cache.join(hex::encode(digest)));
    let staged = std::fs::read(real.join(hex::encode(digest))).expect("slot in real dir");
    assert_eq!(staged, bytes);
    // Nothing passed through the swapped path: the attacker dir
    // holds no slot, no temp, nothing.
    let entries: Vec<_> = std::fs::read_dir(&attacker)
        .expect("read attacker")
        .flatten()
        .collect();
    assert!(entries.is_empty(), "attacker dir untouched: {entries:?}");
    // And the swapped path itself refuses a fresh guard (symlink
    // at the cache path never becomes a guard).
    let error = prepare_cache_dir(&cache).expect_err("swapped path must refuse");
    assert!(error.to_string().contains("symlink"), "got: {error}");
}
