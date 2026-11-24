# Releasing

Releases are automated. On every push to `main`,
`.github/workflows/bump.yml` ("Auto Release") runs the tests, bumps the
version in `Cargo.toml`, commits it to `main`, and tags it. The tag starts
`.github/workflows/release.yml`, which builds `gse` for four targets and
publishes the GitHub release with the archives, their SHA-256 checksums, and
the install scripts.

| Target | Runner | Archive |
| --- | --- | --- |
| `x86_64-unknown-linux-musl` | `ubuntu-24.04` | `.tar.gz` (static) |
| `aarch64-unknown-linux-musl` | `ubuntu-24.04-arm` | `.tar.gz` (static) |
| `aarch64-apple-darwin` | `macos-14` | `.tar.gz` |
| `x86_64-pc-windows-msvc` | `windows-latest` | `.zip` |

## Day-to-day

Merge to `main`. The version bumps by a **patch** by default. Control it
through the head commit message; a merge commit carries the pull request's
title:

- `#minor` / `#major`: bump that level instead.
- `[skip release]`: release nothing for this push.

**Actions → Auto Release → Run workflow** also releases, at a level you pick.
**Actions → Release → Run workflow** rebuilds and republishes an existing
`vX.Y.Z` tag.

The first release, with no `v*` tag yet, ships `Cargo.toml`'s version as it
stands.

## One-time setup

`scripts/setup-github.sh` applies these settings with the `gh` CLI, matching
`danthegoodman1/inodedb`:

- **The `main` ruleset** (`.github/ruleset-main.json`) blocks deleting `main`
  and force pushes to it, and takes changes only through pull requests,
  merged or squashed, whose `lint`, `test (ubuntu-latest)`,
  `test (ubuntu-24.04-arm)`, `test (macos-latest)`, and `windows` checks pass.
  Only deploy keys bypass it.
- **The `Auto Release` deploy key** has write access, and its private half is
  the `RELEASE_DEPLOY_KEY` secret. `bump.yml` checks out with it and pushes
  the `chore(release): vX.Y.Z` commit and the tag over SSH, past the ruleset.
  A push with a deploy key starts workflows, where one with `GITHUB_TOKEN`
  would not: the tag starts `release.yml`, and the commit starts `bump.yml`
  again, which skips it by its message.
- **Fork pull requests** from anyone outside the repository wait for a
  maintainer's approval before their workflows run. Workflows on fork pull
  requests never see the secret.
- **Workflow tokens** default to read-only.

To replace the deploy key, delete it under Settings → Deploy keys and run the
script again.
