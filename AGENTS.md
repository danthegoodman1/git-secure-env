# Working on gse

gse is a Rust CLI (`src/`, binary `gse`) that commits `.env` files to git
encrypted with age. [DESIGN.md](DESIGN.md) is the design; [README.md](README.md)
is the user guide.

- `cargo test` runs unit tests and `tests/integration.rs`, which drives the
  real `git` binary through its hooks with isolated homes and global configs.
  Add an integration test for any behavior that crosses git.
- CI runs `cargo fmt --all --check` and
  `cargo clippy --all-targets -- -D warnings`; run both before committing.
- The toolchain is pinned in `rust-toolchain.toml`.
- Plaintext secrets never go to stdout except through `gse export`,
  `gse textconv`, and the secret key `gse grant --new-key` prints.
- Hooks fail open: they block a commit only for an env file staged in
  plaintext or one with unresolved conflict markers.
- Releases are automatic on merge to `main`; see [RELEASING.md](RELEASING.md).
