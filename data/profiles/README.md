# Profile examples

`default.toml` and `opencode.toml` are examples, not per-system profiles.
They compile into the binary (`BAKED_PROFILES` in `src/profile.rs`;
register any new file there) and seed `${XDG_CONFIG_HOME}/cistella/profiles/`
when a named lookup reaches the XDG/baked default tier (lookups served by
explicit paths or supplied configuration directories never seed). Edit user copies under XDG, never these files'
installed copies.
