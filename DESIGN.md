# git-secure-env (gse) design

gse lets you commit `.env*` files to git encrypted. Plaintext files stay
gitignored. Global git hooks encrypt them into `.gse/` on commit and decrypt
them on pull, checkout, and rebase, merging when both sides changed.

Each developer gets a personal copy readable only by them, so everyone keeps
their own `.env.local` without path conflicts. Shared scopes (`team`, `ci`, …)
hold files encrypted to a group, and stacks layer personal values over shared
ones.

## Concepts

- **Tracked file:** a gitignored plaintext file matched by a pattern in
  `.gse/config.toml`, such as `.env.local`.
- **Scope:** the owner of an encrypted copy. The `user` scope gives each
  developer a directory of their own; a shared scope (`team`, `ci`) has one
  directory and a recipient list.
- **Recipient:** a public key that can decrypt a scope: an age X25519 key, an
  `ssh-ed25519` key, or an `ssh-rsa` key.
- **Identity:** a private key on this machine: the gse age key, an SSH key file,
  or the `GSE_IDENTITY` environment variable in CI.
- **Stack:** an ordered list of plaintext files that `gse run` and
  `gse export` layer, with later files overriding earlier ones.

## Repository layout

```
.gse/
  README.md                    # explains the directory to humans and agents
  config.toml                  # tracked patterns, scopes, stacks
  .gitattributes               # gse merge and diff drivers, LF endings
  users/
    dan/
      recipients.txt           # dan's public keys
      .env.local.age
      apps/web/.env.local.age
    alice/
      recipients.txt
      .env.local.age
  shared/
    team/
      recipients.txt           # team members' keys
      .env.age
    ci/
      recipients.txt           # CI key + maintainers
      .env.ci.age
```

An encrypted copy lives at `.gse/<users/handle | shared/scope>/<repo-relative
path>.age`. Nested paths handle monorepos.

Encrypted files use the standard age format with ASCII armor, so
`age -d -i <identity> <file>` decrypts them without gse.

### Encrypted payload

Each ciphertext carries its recipient list inside the encryption, as a comment
header that gse strips on decrypt:

```
# gse-recipients v1
# ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAA... @github:danthegoodman1
# age1qyqszqgpqyqszqgpqyqszqgpqyqszqgpqyqszqgpqyqszqgpqyqs3290gq @key:github-actions
# gse-end
API_URL=http://localhost:3000
...
```

The header is dotenv comments, so `age -d` output still loads as an env file.

### `.gse/config.toml`

```toml
version = 1

# Each developer's own copy → .gse/users/<handle>/
[[file]]
path = "**/.env.local"
scope = "user"

# Shared with the team → .gse/shared/team/
[[file]]
path = ".env"
scope = "team"

[[file]]
path = ".env.ci"
scope = "ci"

# Later files override earlier ones. Paths resolve against the current directory.
[stacks]
default = [".env", ".env.local"]
ci = [".env", ".env.ci"]
```

`path` is a glob relative to the repository root. `user` is a reserved scope
name. `gse track` and `gse untrack` edit this file.

### `recipients.txt`

A valid age recipients file (`age -R` accepts it). Owner comments group keys:

```
# gse recipients for scope "team"

# @github:danthegoodman1 (refreshed 2026-11-24)
ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAA...
ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAAB...
# unsupported by age: ecdsa-sha2-nistp256 AAAAE2VjZHNh...

# @github:alice (refreshed 2026-11-24)
ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAA...

# @key:github-actions (added 2026-11-24)
age1qyqszqgpqyqszqgpqyqszqgpqyqszqgpqyqszqgpqyqszqgpqyqs3290gq
```

An owner line has the form `# @<kind>:<name>`, where `kind` is `github` or
`key`. Keys below it belong to that owner until the next owner line. gse
records unsupported GitHub keys as comments so `gse refresh` reports only real
changes.

`recipients.txt` mirrors the list embedded in the scope's ciphertexts (see
[Recipient lists](#recipient-lists)). Editing it by hand changes nothing gse
encrypts to.

### `.gse/.gitattributes`

```
*.age text eol=lf merge=gse diff=gse
recipients.txt text eol=lf
```

LF endings keep `core.autocrlf` on Windows from changing a file's bytes, which
would break the comparison with the sync base.

Attributes in a subdirectory apply only to that subtree, so gse needs no global
attributes file. Clones without gse fall back to git's built-in diff and merge.

### `.gse/README.md`

`gse init` writes this file and later versions of gse refresh it. It tells
humans and LLM agents what the directory is and how to work with it:

```markdown
<!-- Managed by gse. `gse init` regenerates this file. -->
# .gse: encrypted env files

This directory holds encrypted copies of gitignored env files such as
`.env.local`. [git-secure-env](https://github.com/danthegoodman1/git-secure-env)
(`gse`) manages it: git hooks encrypt the files on commit and decrypt them on
pull, checkout, and rebase.

## Working here (humans and agents)

- Edit the plaintext file (for example `.env.local`). gse rewrites the `.age`
  files here.
- Keep plaintext env files out of git; they are gitignored on purpose.
- Keep secret values out of logs, commit messages, and chat transcripts.
- `gse status` lists tracked files, their scopes, and pending changes.
- `gse sync` encrypts and stages changed env files. `gse commit` also commits
  them, which you need when an env file is the only change.
- `gse run -- <cmd>` runs a command with the layered environment.
- `gse --help` and `gse <command> --help` document every command.

## Layout

- `config.toml`: which files gse tracks and the scope each belongs to.
- `users/<handle>/`: one developer's personal files, readable only by them.
- `shared/<scope>/`: files shared with a group, such as `team` or `ci`.
- `*/recipients.txt`: public keys that can decrypt each scope, grouped by owner.

The `.age` files use the standard [age](https://age-encryption.org) format, so
`age -d -i <identity> <file>` also decrypts them.
```

## Agent setup

An agent can set gse up from one pasted prompt, and every repository using gse
tells future agents about it in `AGENTS.md`.

### Setup prompt in the project README

The gse README carries this prompt for users to paste into Claude or another
coding agent:

````markdown
Set up git-secure-env (gse) in this repository: https://github.com/danthegoodman1/git-secure-env

1. Run `gse --version`. If gse is missing, ask me before installing it, then
   follow the install steps in its README.
2. Run `gse doctor`. If it reports missing hooks, run `gse install` (it adds
   global git hooks), then run `gse doctor` again.
3. If it reports no identity, ask whether I have one from another machine.
   Import it with `gse keygen --import`, or create one with `gse keygen`. Keep
   the identity's contents out of this conversation.
4. Run `gse init`. If the repository has no `.gse/` yet, first ask me which env
   files to track.
5. Run `gse status` and confirm each tracked env file is decrypted and in sync.
6. Tell me what changed, including the gse section `gse init` added to AGENTS.md.
````

### `AGENTS.md` section

`gse init` writes this section to `AGENTS.md`, creating the file if needed, or
to `CLAUDE.md` when that is the repository's only agent file. Markers let later
runs of `gse init` refresh it in place:

```markdown
<!-- gse:start (managed by `gse init`) -->
## Environment variables

This repo uses [git-secure-env](https://github.com/danthegoodman1/git-secure-env)
(`gse`) to store env files encrypted in `.gse/`. Plaintext files such as
`.env.local` stay gitignored; git hooks encrypt them on commit and decrypt them
after pull, checkout, and rebase.

- Before working with env files, run `gse --version`. If gse is missing, ask
  the user before installing it, then run `gse install`, `gse doctor`, and
  `gse sync`.
- Edit the plaintext env files; gse rewrites the `.age` files in `.gse/`.
- When env files are the only change, commit with `gse commit`.
- Keep secret values out of logs, commits, and chat.
- More: `.gse/README.md` and `gse --help`.
<!-- gse:end -->
```

`gse init` in a repository that already has `.gse/` refreshes `.gse/README.md`
and the `AGENTS.md` section, then runs `gse sync`. `gse doctor` ends with a
round-trip self-test: it commits an env file in a temporary repository, clones
it, and checks that the clone decrypts the same content.

## Machine layout

```
~/.config/gse/                 # $XDG_CONFIG_HOME/gse when set, on macOS too
  config.toml                  # handle, optional GitHub username
  identity.txt                 # age secret key, mode 0600
  recipients.txt               # personal recipients (same format as above)
  install.toml                 # hook mode and any core.hooksPath gse replaced
  hooks/                       # hook shims, git < 2.54 only
```

```toml
# ~/.config/gse/config.toml
handle = "dan"                 # defaults to $USER; `gse keygen --handle` overrides
github = "danthegoodman1"      # optional; adds your GitHub SSH keys as personal recipients
```

Per-worktree state lives at `$(git rev-parse --git-path gse)`, so each worktree
tracks its own plaintext files:

```
.git/gse/
  base/<scope path>.age        # last-synced ciphertext, the merge base
  conflicts                    # plaintext files waiting for conflict resolution
  backup/                      # unsynced edits a conflicting merge replaced
  tmp/                         # staging for atomic plaintext writes
  lock                         # serializes concurrent gse runs
```

gse stores the merge base as ciphertext, so `.git/` holds no extra plaintext.

## Keys and identities

### Personal identity

`gse keygen` creates an age X25519 identity in `~/.config/gse/identity.txt`
and writes `~/.config/gse/config.toml`. It prompts for a handle, defaulting to
`$USER`; `--handle` sets it directly. Handles match `[a-z0-9._-]+`.

Run on a machine that already has an identity, `gse keygen --handle` or
`--github` updates the configuration and keeps the key; `--force` replaces the
key.

Each person has one identity and copies it to each machine
(`gse keygen --import <file|->`), the same way they would copy an SSH key, so
every machine encrypts personal files to the same recipients. Back it up in a
password manager: losing it loses every personal file.

Personal recipients come from `~/.config/gse/recipients.txt`, which holds the
identity's public key plus, when `github` is set, your GitHub SSH keys. Any
machine with one of those SSH private keys can then decrypt your files.

### Identity discovery

To decrypt, gse tries these identities in order:

1. `GSE_IDENTITY`: one or more secret keys in the variable's value (CI).
2. `GSE_IDENTITY_FILE`: path to an identity file.
3. `~/.config/gse/identity.txt`.
4. Each `~/.ssh/*` private key whose `.pub` matches a recipient of the file.

gse tries the keys that need no passphrase first. It loads an SSH key only
when those fail and the key's tag matches a stanza in the file's header, so a
passphrase prompt appears only for a file that key can open. Prompts go to the
terminal; with none (GUI git clients), gse skips that key and warns.
`GSE_NO_SSH=1` turns SSH keys off.

### Key types

age decrypts with `ssh-ed25519` and `ssh-rsa` private key files. It cannot use:

- ECDSA keys.
- Keys held by an agent (1Password, Secretive, YubiKey `sk-` keys). The agent
  protocol only signs; age needs the private key.

A person whose GitHub keys all fall in those groups needs a gse age key added
with `gse grant --key`. Hardware-backed keys come later through age plugins
(`age-plugin-yubikey`, `age-plugin-se`).

### Granting and revoking access

- `gse grant <github-user>... --scope team` fetches
  `https://github.com/<user>.keys`, writes an owner block, re-encrypts every
  file in the scope, and stages the changes.
- `gse grant --key <pubkey> --name <label> --scope ci` adds a key directly.
- `gse grant --new-key github-actions --scope ci` generates a key pair, adds
  the public key, and prints the secret key once for a CI secret.
- `gse revoke <name> --scope team` removes the owner block and re-encrypts
  the scope.
- `gse refresh [--scope team | --me]` re-fetches every `@github` owner, updates
  the blocks, re-encrypts changed scopes, and reports added and removed keys.

A scope's recipient list lives inside its files, so changing access needs a
scope with at least one file: `gse track .env.ci --scope ci`, then
`gse grant ... --scope ci`. A new scope's first file encrypts to your own keys.

age generates a fresh file key for every encryption, so re-encryption cuts a
removed recipient off from new versions. Git history keeps the old versions,
so gse warns after every removal: rotate the secrets.

Only these explicit commands touch the network. Hooks never do.

## Recipient lists

Hooks encrypt automatically, so the list they encrypt to must come from someone
who can already decrypt. gse takes it from inside the ciphertext:

- **Shared scopes:** hooks re-encrypt to the list embedded in the current
  ciphertext. Only `gse grant`, `gse revoke`, and `gse refresh` change that
  list, and each rewrites every file in the scope, which requires decrypting
  them. Someone who can push but cannot decrypt can edit `recipients.txt`, and
  hooks ignore the edit and warn that the mirror drifted.
- **Personal scope:** hooks encrypt to `~/.config/gse/recipients.txt`. The
  committed `users/<handle>/recipients.txt` shows others which keys can read
  the directory and detects handle collisions: gse refuses to write into a user
  directory whose embedded list excludes your key. If the embedded list holds
  keys missing locally (you refreshed on another machine), hooks warn and
  suggest `gse refresh --me`.
- **Reporting:** inbound sync compares the embedded lists in the base and the
  pulled ciphertext and prints each added or removed key with the commit and
  author that changed it.

Push access stays trusted, as it is for the rest of the repository. Someone with
push access can still replace a ciphertext outright, wiping content they cannot
read and listing their own key. Inbound sync reports that as a recipient change.
Verifying commit signatures on `.gse/` changes would close that gap and can come
later.

## Sync

### Outbound: pre-commit and `gse sync`

For each tracked plaintext file `P` in a scope you can write:

1. If `P` contains conflict markers, block the commit.
2. If the committed ciphertext changed since the base (pulled but not yet
   synced), run inbound sync for `P` first.
3. If `P` matches the decrypted ciphertext and the recipient list is unchanged,
   skip it. age output is randomized, so re-encrypting unchanged content would
   rewrite the file on every commit.
4. Otherwise encrypt `P`, write the `.age` file, `git add` it, and update the
   base.

A plaintext file that disappears stays in git: inbound sync restores it.
`gse untrack` removes a file from gse.

`git commit` runs pre-commit and then reports "nothing to commit" when the
ciphertext is the only change: the hook's staging lands in the next commit.
`gse commit` runs `gse sync` and then `git commit -- .gse/` to cover that case.

gse shells out to `git` for index and object access. `git commit -a` and
`git commit <path>` give hooks a temporary index through `GIT_INDEX_FILE`, and
the `git` binary honors it automatically. With `git commit <path>`, ciphertext
staged in that temporary index reaches the commit but not the real index, so a
post-commit hook stages it again; otherwise the next commit would revert it.

### Inbound: post-checkout, post-merge, post-rewrite

For each ciphertext `C` in your user directory and each shared scope you can
decrypt:

1. If `C` equals the base, skip it.
2. Decrypt `C` and the base. Report any change between their embedded
   recipient lists.
3. If `P` is missing or matches the base, write the decrypted `C` to `P`.
4. If `P` matches the decrypted `C`, only update the base.
5. Otherwise run a 3-way line merge (base, `P`, `C`). A clean merge writes `P`.
   A conflict writes `P` with markers and sets the conflict flag; outbound sync
   blocks until you resolve it.
6. Set the base to `C`.

gse writes plaintext with mode 0600 through a temporary file in `.git/gse/` and
a rename. Post-checkout skips inbound sync while a rebase, merge, or cherry-pick
is in progress, because it fires mid-rebase; post-rewrite handles the end.
Post-merge always syncs: git still holds `MERGE_HEAD` while it runs.

Inbound sync runs on branch switches too, so a tracked plaintext file follows
the checked-out branch the way a tracked file would.

Observed hook events (git 2.53):

| Operation | Hooks |
|---|---|
| `git clone` | post-checkout |
| `git pull` (merge or fast-forward) | post-merge |
| `git pull --rebase` with nothing to replay | post-merge |
| `git pull --rebase` replaying local commits | post-checkout, then post-rewrite |
| `git switch`, `git checkout` | post-checkout |
| `git commit` | pre-commit, then post-commit |

### Merge driver

`gse install` sets these in global config:

```
merge.gse.name = gse encrypted env merge
merge.gse.driver = gse merge-driver %O %A %B %P
diff.gse.textconv = gse textconv
```

Git calls the driver when both sides changed the same `.age` file during a
merge, rebase, or cherry-pick. It decrypts all three versions and merges them
line by line:

- **Clean:** encrypt the result into `%A` and exit 0. The post-merge or
  post-rewrite hook then decrypts it into `P`, merging any unsynced edits.
- **Conflict:** write `P` with markers, leave "ours" in `%A`, record it as the
  base, exit 1, and print the next steps: resolve `P`, run `gse sync` (staging
  the `.age` file resolves the index conflict), then `git commit` or
  `git rebase --continue`. Unsynced edits in `P` go to `.git/gse/backup/`
  first.
- **Not a recipient:** keep "ours", exit 1, and name who can merge it.

When both sides changed the embedded recipient list (two members granted access to
different people), the merged list applies both sides' additions and removals
to the base list.

`gse textconv` decrypts for `git diff` and `git log -p`. For files you cannot
decrypt, it prints `<encrypted; not a recipient>`.

### Safety checks

Hooks block a commit only to stop a plaintext leak:

- A staged path matches a tracked plaintext pattern (`git add -f .env.local`).
- A tracked plaintext file contains conflict markers.

Everything else warns and lets the commit through: a missing identity, a
decrypt failure, a `recipients.txt` that drifted from the embedded list, a
tracked file that is not gitignored, a `#personal` key given a value in a
shared file. `GSE_SKIP=1` skips gse for one command.
`gse init` and `gse track` add ignore rules; `gse doctor` reports gaps.

Hooks exit within a millisecond or two in repositories without
`.gse/config.toml`.

## Hook installation

`gse install` runs once per machine. It checks `git version` and picks a mode.

### Git 2.54 and later: config-based hooks

```
hook.gse-pre-commit.command = command -v gse >/dev/null 2>&1 || exit 0; gse hook pre-commit
hook.gse-pre-commit.event = pre-commit
```

and the same for `post-commit`, `post-checkout`, `post-merge`, and
`post-rewrite`. Git runs the command through `sh -c` and appends `"$@"` when
the hook has arguments, which this form accepts. The `command -v` guard lets
commits through if gse is uninstalled without `gse uninstall`.

Config hooks run alongside husky, lefthook, and repository hooks; the
traditional hook script runs last. Friendly names cannot equal an event name,
hence the `gse-` prefix.

### Before 2.54: global `core.hooksPath`

`gse install` sets `core.hooksPath = ~/.config/gse/hooks` and writes a shim for
every hook event git supports, because a global hooks path replaces each
repository's `.git/hooks`. Each shim runs gse (for the five gse events) and then
chains to the hook that would have run without gse:

```sh
#!/bin/sh
if command -v gse >/dev/null 2>&1; then
  gse hook pre-commit "$@" || exit $?
fi
hook="$(git rev-parse --git-common-dir 2>/dev/null)/hooks/pre-commit"
if [ -x "$hook" ]; then exec "$hook" "$@"; fi
exit 0
```

If a global `core.hooksPath` existed before, `gse install` records it and the
shims chain there instead. gse leaves stdin unread so chained hooks such as
post-rewrite still receive it.

A repository-local `core.hooksPath` (husky sets one) overrides the global one,
so gse hooks never run there. `gse doctor` detects this and prints the line to
add to the repository's hooks, or recommends git 2.54.

`gse doctor` also suggests `gse install` after a git upgrade past 2.54, which
migrates from shims to config hooks. `gse uninstall` restores the previous
configuration.

## Layering

Stacks layer plaintext files at read time; gse never writes a merged file:

```
gse run -- npm run dev             # stack "default": .env, then .env.local
gse run --stack ci -- npm test     # .env, then .env.ci
gse export --stack default         # print the layered result (dotenv, shell, json)
```

Name the team file `.env` and the personal file `.env.local` when possible.
Next.js, Vite, and dotenv-flow already load `.env.local` over `.env`, so they
get the same layering without `gse run`.

A shared file can mark keys each developer must supply, as EpicEnv does:

```
DATABASE_URL= #personal
AWS_PROFILE= #personal
```

Dotenv loaders read these as empty values. `gse run` and `gse export` warn when
no later layer sets a `#personal` key, and gse warns when it encrypts a shared
file that gives a `#personal` key a value.

## CI

CI is a recipient like any other:

```
gse track .env.ci --scope ci                    # the scope's first file
gse grant --new-key github-actions --scope ci   # prints AGE-SECRET-KEY-1...
```

Store the secret key as the `GSE_IDENTITY` repository secret, then:

```yaml
- uses: actions/checkout@v5
- run: curl -fsSL https://github.com/danthegoodman1/git-secure-env/releases/latest/download/install.sh | sh
- run: gse decrypt --scope ci
  env:
    GSE_IDENTITY: ${{ secrets.GSE_IDENTITY }}
```

`gse run --stack ci -- <cmd>` decrypts in memory instead, for a stack declared
in `.gse/config.toml`. CI needs no hooks: these commands read `.gse/` directly.

## CLI

| Command | Purpose |
|---|---|
| `gse install`, `gse uninstall` | Set up or remove global hooks and drivers |
| `gse doctor` | Check git version, hooks, ignore rules, identities; run a round-trip self-test |
| `gse keygen [--handle h] [--github u] [--import f]` | Create or import the personal identity |
| `gse init` | Create `.gse/`, track gitignored `.env*` files, write `.gse/README.md` and the `AGENTS.md` section; re-run to refresh them and sync |
| `gse track <glob> [--scope s]`, `gse untrack <glob>` | Edit tracked patterns |
| `gse status` | Tracked files, scopes, pending changes, conflicts, recipient drift |
| `gse sync` | Inbound then outbound sync; stages `.gse/` changes |
| `gse commit [-m msg]` | `gse sync`, then commit only `.gse/` |
| `gse grant`, `gse revoke`, `gse refresh` | Manage shared-scope recipients |
| `gse run [--stack s] -- <cmd>` | Run a command with the layered environment |
| `gse export [--stack s] [--format f]` | Print the layered environment |
| `gse decrypt [--scope s]` | Write plaintext files (CI, recovery) |
| `gse hook <event>` | Hook entry point (plumbing) |
| `gse merge-driver %O %A %B %P` | Merge driver (plumbing) |
| `gse textconv <file>` | Diff text conversion (plumbing) |

## Implementation

### Crates

- `age` with `ssh` and `armor` features (pure Rust; `plugin` later)
- `clap` (derive), `anyhow`
- `serde`, `toml`
- `globset` for tracked patterns
- `diffy` for 3-way line merges with conflict markers
- `ureq` with rustls for GitHub key fetches
- `rpassword` for SSH key passphrases
- `sha2`, `base64` to match SSH keys to age stanza tags
- Tests: `tempfile`

File locking uses `std::fs::File::lock`.

### Modules

```
src/
  main.rs          CLI definition and dispatch
  config.rs        .gse/config.toml and ~/.config/gse/config.toml
  recipients.rs    recipients.txt parsing and writing, owner blocks
  identity.rs      identity discovery and lazy SSH keys
  crypto.rs        age encrypt/decrypt with armor
  payload.rs       embedded recipient header
  github.rs        github.com/<user>.keys fetch and filtering
  git.rs           git subprocess helpers
  state.rs         .git/gse state, base copies, lock
  sync.rs          inbound and outbound sync, status
  merge.rs         3-way merge, merge driver, textconv
  access.rs        grant, revoke, refresh
  setup.rs         keygen, init, track, untrack, commit, decrypt
  dotenv.rs        parsing, stacks, run, export
  hooks.rs         install, uninstall, doctor, shims, hook dispatch
  templates.rs     .gse/README.md, AGENTS.md section, .gitattributes
  ui.rs, util.rs   messages and file helpers
```

### Testing

Integration tests (`tests/integration.rs`) drive the real `git` binary in
temporary directories. Each simulated machine has its own `HOME` and
`GIT_CONFIG_GLOBAL`. Scenarios cover clones of one bare remote (one person on
two machines, two people, CI) through commit, partial commit, pull (merge,
rebase), conflicts, grants by GitHub username with SSH keys, CI keys,
revocation, `recipients.txt` edits by someone outside the scope, handle
collisions, stacks, textconv, and the doctor self-test.

CI runs them on Linux x86_64, Linux arm64, and macOS with each runner's git:
config-based hooks on git 2.54 and later, shims before, and one test that
always covers the shims. The harness is Unix-only; Windows CI builds gse and
runs its unit tests.

### Release

Every merge to `main` releases: `bump.yml` tests, bumps the patch version
(`#minor` or `#major` in the merge title bumps more), commits, and tags with a
deploy key that bypasses the `main` ruleset. The tag starts `release.yml`,
which builds on native runners:

| Target | Runner |
|---|---|
| `x86_64-unknown-linux-musl` (static) | `ubuntu-24.04` |
| `aarch64-unknown-linux-musl` (static) | `ubuntu-24.04-arm` |
| `aarch64-apple-darwin` | `macos-latest` |
| `x86_64-pc-windows-msvc` | `windows-latest` |

The release carries the archives, their SHA-256 checksums, `install.sh`, and
`install.ps1`. `RELEASING.md` covers the repository settings, which
`scripts/setup-github.sh` applies. The crate is `git-secure-env` with binary
`gse`.

## Status

Implemented: everything above.

Planned:

- Hardware-backed keys through age plugins (`age-plugin-yubikey`,
  `age-plugin-se`).
- Commit signature checks on `.gse/` changes.
- A Homebrew tap and crates.io publishing.
- Integration tests on Windows.
