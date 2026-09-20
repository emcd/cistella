# Profile examples

`opencode.toml` is the sole baked example, not a per-system profile. It
compiles into the binary (`BAKED_PROFILES` in `src/profile.rs`; register
any new file there) and seeds `${XDG_CONFIG_HOME}/cistella/profiles/`
when a named lookup reaches the XDG/baked default tier (lookups served by
explicit paths or supplied configuration directories never seed). Edit user copies under XDG, never these files'
installed copies.

The minimal `default.toml` sleep-harness profile is a test fixture, not
a shipped example: it lives at `tests/data/profiles/default.toml` and
live tests conduct against it by explicit path (never by baked name).
