# mounts Specification Delta

## ADDED Requirements

### Requirement: Nested-under-RO availability preflight
Overlap validation admits descendant triples over read-only ancestors (deepest mount wins); the OCI runtime honors them only when the intermediate chain pre-exists in the ancestor's host source (preexists rule: the child bind resolves through the mounted parent against the host tree, so a missing link fails `mkdirat` inside the RO mount). After mount merge and canonicalization, before unit or scratch creation, `conduct` SHALL translate each descendant-under-RO container suffix onto its nearest already-ordered RO ancestor's canonical host source and require every chain component to exist as a directory; a missing link or wrong type is a typed error naming the RO ancestor and the missing translated host path, leaving no residue. Runc remains authoritative for host-tree races with existing fail-closed teardown covering them.

#### Scenario: Preexisting nested chain succeeds
- **WHEN** a session nests a target beneath an RO ancestor whose host source already contains the full intermediate chain
- **THEN** conduct proceeds, the child is writable, the parent stays RO

#### Scenario: Missing intermediate refuses pre-mutation
- **WHEN** any intermediate link is absent (or not a directory) in the ancestor's host source
- **THEN** conduct fails with a typed error naming the RO ancestor and the missing host path, before unit file or scratch creation (assert both absent)

#### Scenario: Symlinked sources resolve before translation
- **WHEN** an ancestor host source traverses a symlink
- **THEN** the canonicalized source supplies the namespace and the check passes or fails on the resolved tree
