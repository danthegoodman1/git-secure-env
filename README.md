# git-secure-env

git-secure-env (`gse`) commits your `.env` files to git, encrypted. Plaintext
env files stay gitignored. Global git hooks encrypt them into `.gse/` on
commit, and decrypt them on pull, checkout, and rebase, merging when both
sides changed. Your secrets follow the repository to every machine and
teammate that should have them.

```
      laptop                          git                         desktop
 ┌──────────────┐   commit   ┌───────────────────┐    pull    ┌──────────────┐
 │ .env.local   │ ─────────> │ .gse/users/alice/ │ ─────────> │ .env.local   │
 │ (gitignored) │  encrypt   │   .env.local.age  │  decrypt   │ (gitignored) │
 │              │            │ (encrypted)       │  + merge   │              │
 └──────────────┘            └───────────────────┘            └──────────────┘
```

**Project status: early.** gse works on macOS, Linux, and Windows; its
end-to-end tests run on macOS and Linux. Commands and the `.gse/` layout may
change before 1.0.

## Set up with an agent

Most people set gse up through their coding agent. Paste this into Claude
Code, Codex, or any other coding agent:

````text
Set up git-secure-env (gse) in this repository: https://github.com/danthegoodman1/git-secure-env

1. Run `gse --version`. If gse is missing, ask me before installing it with:
   - macOS/Linux: curl -fsSL https://github.com/danthegoodman1/git-secure-env/releases/latest/download/install.sh | sh
   - Windows: irm https://github.com/danthegoodman1/git-secure-env/releases/latest/download/install.ps1 | iex
   If `cargo` is installed, offer to build from source instead:
   cargo install --locked --git https://github.com/danthegoodman1/git-secure-env
   If `gse` is not found right after installing, run it by the full path the
   installer printed.
2. Run `gse doctor`. If it reports missing hooks, run `gse install`, which adds
   global git hooks, and run `gse doctor` again.
3. If it reports no identity, ask whether I have one from another machine. If
   I do, have me copy it here and run `gse keygen --import <file>`. If not, ask
   me for a handle (default: my username) and run `gse keygen --handle <handle>`.
   Never read or print the identity's contents. Remind me to back up
   ~/.config/gse/identity.txt in a password manager.
4. Run `gse init`. It tracks the repository's untracked `.env*` files as my
   personal files, gitignores them, and adds a short gse section to AGENTS.md.
5. Run `gse status` to confirm each file is encrypted. Show me the staged
   changes and ask before committing.
````

The section `gse init` adds to `AGENTS.md` tells later agent sessions how the
repository's env files work.

Use an agent on your own machine. An agent in a cloud VM would leave gse and
your identity on a throwaway machine; give it a key of its own, as with
[CI](#ci).

## Features

**Plain git.** Commit, push, and pull as usual. The hooks do the encrypting,
decrypting, and staging.

**Personal files.** Each developer's `.env.local` lives in
`.gse/users/<handle>/`, readable only by them and synced across their
machines. Teammates' files never collide.

**Team and CI scopes.** Shared scopes such as `.gse/shared/team/` hold files
encrypted to a group. Grant access by GitHub username, and gse fetches their
SSH keys. CI gets a scope and a key of its own.

**Merges.** When both sides change an env file, gse merges the plaintext and
leaves conflict markers where edits overlap. `git diff` shows plaintext diffs.

**Layering.** Personal values override team values. `gse run -- <cmd>` runs a
command with `.env` layered under `.env.local`.

**Fails safe.** Hooks block a commit that stages an env file in plaintext or
one with unresolved conflict markers. Anything else warns and lets the commit
through.

**Standard encryption.** Files use [age](https://age-encryption.org), so
`age -d` decrypts them without gse.

**Cross-platform.** One binary for macOS (Apple Silicon), Linux (x86_64,
arm64), and Windows (x86_64).

## Example

On your laptop, edit an env file, then commit and push with your code:

```sh
$ echo 'STRIPE_KEY=sk_test_123' >> .env.local
$ git commit -am "Add billing"
gse: encrypted .env.local -> .gse/users/alice/.env.local.age
$ git push
```

On your desktop, pull:

```sh
$ git pull
gse: updated .env.local
$ grep STRIPE .env.local
STRIPE_KEY=sk_test_123
```

When an env file is the only change, run `gse commit`. Git decides there is
nothing to commit before the hook stages the ciphertext.

## Installation

macOS (Apple Silicon) and Linux (x86_64, arm64):

```sh
curl -fsSL https://github.com/danthegoodman1/git-secure-env/releases/latest/download/install.sh | sh
```

Windows (PowerShell):

```powershell
irm https://github.com/danthegoodman1/git-secure-env/releases/latest/download/install.ps1 | iex
```

The installer puts gse in `~/.local/bin` and, when that directory is missing
from PATH, adds it in your shell's startup file. `GSE_NO_MODIFY_PATH=1` prints
the line to add instead. The Windows installer uses `%LOCALAPPDATA%\gse\bin`
and adds it to your user PATH.

From source, on any platform Rust supports:

```sh
cargo install --locked --git https://github.com/danthegoodman1/git-secure-env
```

Then, once per machine:

```sh
gse install   # global git hooks, merge driver, and diff textconv
gse keygen    # create your identity, or `--import <file>` to reuse one
gse doctor    # check the setup and run a round-trip self-test
```

Back up `~/.config/gse/identity.txt` in a password manager. Losing it loses
your personal files.

## Usage

### Track a repository

```sh
cd my-repo
gse init                            # track the .env* files it finds
git commit -m "Track env files with gse"
```

`gse init` writes `.gse/` (config, a README for humans and agents, git
attributes), gitignores the env files it tracks, and adds a gse section to
`AGENTS.md`. Templates such as `.env.example` stay as they are. On a
new clone, the checkout hook decrypts your files.

### Share with a team

```sh
gse track .env --scope team         # .env becomes the team's file
gse sync                            # encrypt it to you, the scope's first member
gse grant alice bob --scope team    # add their GitHub SSH keys
git commit -m "Share .env with the team"

gse refresh                         # re-fetch everyone's GitHub keys
gse revoke bob --scope team         # re-encrypt without bob, then rotate the secrets
```

Members decrypt with an `ssh-ed25519` or `ssh-rsa` private key file in
`~/.ssh`. For a member whose keys live only in an agent (1Password,
Secretive, a YubiKey), grant their gse key instead:
`gse grant --key age1... --name alice --scope team`.

### CI

```sh
gse track .env.ci --scope ci
gse sync
gse grant --new-key github-actions --scope ci   # prints AGE-SECRET-KEY-1...
git commit -m "Add CI env"
```

Store the printed key as the `GSE_IDENTITY` repository secret, then:

```yaml
- run: curl -fsSL https://github.com/danthegoodman1/git-secure-env/releases/latest/download/install.sh | sh
- run: gse decrypt --scope ci   # writes .env.ci
  env:
    GSE_IDENTITY: ${{ secrets.GSE_IDENTITY }}
```

### Layering

Stacks layer files, later ones overriding earlier ones. The default stack is
`.env` then `.env.local`, the order Next.js, Vite, and dotenv-flow use.

```sh
gse run -- npm run dev              # run with the layered environment
gse export --format shell           # or dotenv, json
```

Declare more stacks in `.gse/config.toml`:

```toml
[stacks]
ci = [".env", ".env.ci"]
```

A shared file can mark keys each developer must supply. `gse run` warns when
no later layer sets them:

```sh
DATABASE_URL= #personal
```

### Commands

| Command | Purpose |
| --- | --- |
| `gse install`, `gse uninstall` | Set up or remove global hooks and drivers |
| `gse doctor` | Check the setup; run a round-trip self-test |
| `gse keygen` | Create or import your identity; set your handle |
| `gse init` | Set up `.gse/`; re-run to refresh its managed files and sync |
| `gse track`, `gse untrack` | Change which files gse tracks, and in which scope |
| `gse status` | Each tracked file's scope and sync state |
| `gse sync` | Decrypt incoming changes, encrypt and stage local ones |
| `gse commit` | Sync, then commit only `.gse/` |
| `gse grant`, `gse revoke`, `gse refresh` | Manage who can read a shared scope |
| `gse run`, `gse export` | Use a stack's layered environment |
| `gse decrypt` | Write plaintext files from `.gse/` (CI, recovery) |

`gse <command> --help` documents each one.

## How it works

### Hooks

On git 2.54 and later, `gse install` adds config-based hooks (`hook.gse-*`),
which run alongside husky, lefthook, and repository hooks. Older git gets a
global `core.hooksPath` of shims that run gse, then chain to each
repository's own hooks. A repository-local `core.hooksPath` (husky sets one)
overrides the global shims; `gse doctor` reports it.

The hooks and drivers run gse by the full path `gse install` recorded, so
they work in git clients whose PATH lacks gse, such as IDEs. If gse moves,
they use the gse on PATH; `gse doctor` reports the change.

Hooks block a commit in two cases: an env file staged in plaintext, or one
with unresolved conflict markers. `GSE_SKIP=1` skips gse for one command.

### Security model

- Plaintext stays gitignored. A machine without gse syncs nothing and leaks
  nothing.
- Each ciphertext carries its recipient list inside the encryption. Hooks
  encrypt to that list, so editing `recipients.txt` changes nothing; only
  `grant`, `revoke`, and `refresh`, which require decrypting, change it.
- Pulls report recipient changes with the commit and author.
- Revoking re-encrypts, but git history keeps old versions: rotate the
  secrets.
- Push access stays trusted. Someone who can push can replace a ciphertext
  outright, which wipes its content and shows up as a recipient change.

[DESIGN.md](DESIGN.md) covers the design in depth.

## Development

```sh
cargo test   # unit tests, plus end-to-end tests that drive real git
```

[RELEASING.md](RELEASING.md) describes releases.

## License

[MIT](LICENSE)
