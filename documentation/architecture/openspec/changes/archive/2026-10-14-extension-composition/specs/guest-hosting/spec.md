# guest-hosting Specification

## Purpose
Guest hosting: synced from change extension-composition. Update Purpose after archive.
## MODIFIED Requirements
### Requirement: Profile-declared guest selection

Conduct SHALL host exactly the isolator and extensions a profile declares (`[isolator]` table, `[[extensions]]` array of tables): the named isolator backend conducts the session, and each named extension answers prepare. Absent tables mean pure Podman behavior (2.x path — no extension guest spawned, no hooks staged; the Podman isolator guest itself is still hosted as in 2.x, and 0.1.x profiles run unchanged). Names SHALL resolve first as shipped guests (`podman`, `landlock`, `tmpdir`, sibling-relative discovery as today), then through the seat-scoped guest registry; names with neither a shipped implementation nor a registry entry, and duplicate extension entries, SHALL refuse pre-create with a typed error naming the declaration (registration is opt-in per guest — an unregistered name can never silently plain a meant-hooked session). Any number of declared extensions conducts (each answers prepare independently in declaration order; each raw plan gates against its own negotiated capabilities pre-concat, then the plans concatenate and the single central merge stays atomic under existing validators — collisions across plans refuse deterministically naming all claimants, hook order follows declaration order, lattice evaluation sees each contribution under its own provenance). At most one contributing plan carries guest hooks, and at most one hook request per contributing plan (admission refuses naming all claimants — the wrapper chain is the only true ceiling). Landlock keeps no merge privilege: shipped-vs-registered is resolution-only and merge treats every provenance uniformly.

#### Scenario: Undeclared sessions run plain
- **WHEN** a profile declares no extensions
- **THEN** conduct spawns no extension guest and runs the plain lifecycle (the isolator guest is hosted as usual); no prepare transaction occurs

#### Scenario: Declared Landlock engages hooks
- **WHEN** a profile declares the `landlock` extension
- **THEN** the Landlock guest answers prepare and its hook stages before create, and the session conducts on the hooked path

#### Scenario: Selected-but-empty hooks refuse pre-create
- **WHEN** a profile declares the `landlock` extension but the guest answers zero hooks (malformed, buggy, or replaced guest)
- **THEN** conduct refuses pre-create with a typed error (never a silent plain session); the selection guarantees engagement

#### Scenario: Unknown guest refuses pre-create
- **WHEN** a profile names an isolator or extension with no shipped implementation and no registry entry
- **THEN** conduct refuses pre-create with a typed error naming the declaration; no session starts undeclared-unhooked

#### Scenario: Registered third-party guest resolves
- **WHEN** a profile names a guest with a valid registry entry (verified source, matching role)
- **THEN** conduct hosts it exactly as a shipped guest of that role (prepare for extensions, lifecycle for isolators); verification failures refuse pre-create typed

#### Scenario: Declared tmpdir resolves shipped
- **WHEN** a profile declares the `tmpdir` extension with no registry entry
- **THEN** conduct spawns the sibling `cistella-extension-tmpdir` binary with install-dir trust (no digest verification — same trust as the landlock sibling) and hosts its prepare like any extension

#### Scenario: N extensions compose in declaration order
- **WHEN** a profile declares `landlock` plus two other extensions (shipped or registered), at most one contributing guest hooks and at most one hook request per contribution
- **THEN** all three answer prepare in declaration order, their plans concatenate into one atomic central merge, and the session conducts with all contributions (hooks staged, mounts bound, slices minted)

#### Scenario: Duplicate extension entries refuse
- **WHEN** a profile declares the same extension name twice
- **THEN** conduct refuses pre-create with a typed error naming the declaration; the same guest is never hosted twice
