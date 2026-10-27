# Operation log: durable per-operation decision receipts

Append-only logs of lifecycle decisions, one directory per operation
under `$XDG_STATE_HOME/cistella/logs/<op-id>/` (fallback
`~/.local/state/cistella/logs/`). Logging is strictly beside the
decision: a logging failure is an observability loss only and never
changes a reap, safety, or failure outcome.

## Modules

- `codec.rs` — length-prefixed framing (4-byte big-endian length plus
  JSON, 256 KiB max payload) and the reader. Reads are bounded,
  non-blocking, and verified-regular-file only; a torn tail discards
  that frame and everything after it, preserving the valid prefix and
  counting discards. A missing terminal reads UNKNOWN, never success.
- `admission.rs` — capacity accounting under a nonblocking admission
  lock (contention skips logging). Each open writes an owner-attributed
  reservation before creating its directory; reservations release on
  close/drop, dead owners are reaped, and every unaccountable entry is
  charged a full share rather than zeroed. Refusal fails the open, not
  the business operation.
- `prune.rs` — victim selection (completed plus inactive-incomplete,
  never live writers or finalizing operations) split from eviction so
  an unlink-time recheck under the admission lock can skip victims
  that gained protection after selection. Every eviction leaves an
  accounting marker.
- Root (`operation_log.rs`) — `Operation::{open, close}` lifecycle:
  terminal record, fsync, finalized evidence, then maintenance. A
  contended close skips the terminal entirely (explicit incomplete
  UNKNOWN) rather than finalizing unprotected. `emit.rs` holds the
  record helpers, including gc per-target classify/reap receipts with
  raw `Observed::{NotQueried, Missing, Present}` values.

## Lifecycle of one operation

```
open (reserve capacity under admission lock)
  → start record (seq 0: op id, parent, actor, role, pid, wall clock)
  → body records (decisions and observations, each owned by this writer alone)
  → close(outcome): terminal record → fsync → finalized evidence → prune
```

## Key invariants

- One owning writer per operation file; no multi-writer joins.
- The log root is created with anchored discipline
  (`guest_anchor::open_state_dir_all`): missing ancestors are built
  top-down from the filesystem root with `mkdirat`/`openat` against
  retained verified FDs, so a symlink at any level refuses the open
  and a fresh seat opens its first log.
- An operation is eligible for unlinking only from the fsync onward;
  the finalizing marker plus a live owner protects the window.
- Only a terminal reading exactly `success` counts as success; every
  other completed outcome accrues to the protected failure class.
- Records carry exactly what the deciding path observed (`not-queried`
  / `unavailable` markers, never invented values); the logger issues
  no extra backend queries.
