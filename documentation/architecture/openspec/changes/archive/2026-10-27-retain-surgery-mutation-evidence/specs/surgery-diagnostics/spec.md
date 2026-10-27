## MODIFIED Requirements

### Requirement: Landing history retained for later failures

The successful mutation's identity, bounded landing history, and the pre-surgery baseline SHALL be dumped alongside any failure that occurs after landing succeeded (including the separate exists-but-uninspectable admittability class), without changing any verdict semantics. The mutation identity names the argv as run, the successful exit status, and both streams capped at 4000 kept characters each with explicit truncation markers naming the dropped character count; the section is labelled rendered-and-not-re-runnable. The landing-history collection is scoped to landing-timeout evidence; this proposal extends the retained set on the successful-landing path, and later-failure capture reuses the retained set only, and the gap (no dedicated post-mortem on the later path) is disclosed, not closed by scope expansion.

#### Scenario: Later failure keeps its context

- **WHEN** a post-landing failure occurs
- **THEN** the report appends the retained mutation identity, bounded landing history, and baseline with verdict semantics unchanged

#### Scenario: Mutation identity is bounded and marked

- **WHEN** the retained mutation produced more than 4000 characters on a stream
- **THEN** the rendered section carries the head of the stream plus an explicit truncation marker naming the dropped character count, never the full unbounded stream
