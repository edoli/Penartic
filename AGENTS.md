# Agent Instructions

## Validation

- After making code changes, run `cargo fmt`.
- After Rust code changes, run `cargo build`.
- If `cargo build` is blocked because a running `penartic` process is holding the executable open (for example from a VS Code debug session), terminate the `penartic` process and retry the build.
