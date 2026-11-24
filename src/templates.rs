//! Files gse writes into repositories.

pub const REPO_URL: &str = "https://github.com/danthegoodman1/git-secure-env";

pub const GITATTRIBUTES: &str = "\
# Managed by gse: merge and diff encrypted env files through gse, and keep
# LF line endings so checkouts never change the files' bytes.
*.age text eol=lf merge=gse diff=gse
recipients.txt text eol=lf
";

pub const GSE_README: &str = "\
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
";

pub const AGENTS_START: &str = "<!-- gse:start (managed by `gse init`) -->";
pub const AGENTS_END: &str = "<!-- gse:end -->";

pub fn agents_section() -> String {
    format!(
        "{AGENTS_START}
## Environment variables

This repo uses [git-secure-env]({REPO_URL})
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
{AGENTS_END}
"
    )
}

/// Inserts or replaces the managed section in an agent instructions file.
pub fn upsert_agents_section(existing: &str) -> String {
    let section = agents_section();
    if let (Some(start), Some(end)) = (existing.find(AGENTS_START), existing.find(AGENTS_END))
        && start < end
    {
        let after = &existing[end + AGENTS_END.len()..];
        let after = after.strip_prefix('\n').unwrap_or(after);
        return format!("{}{section}{after}", &existing[..start]);
    }
    if existing.trim().is_empty() {
        return section;
    }
    let sep = if existing.ends_with("\n\n") {
        ""
    } else if existing.ends_with('\n') {
        "\n"
    } else {
        "\n\n"
    };
    format!("{existing}{sep}{section}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agents_section_is_idempotent() {
        let once = upsert_agents_section("# Project\n\nNotes.\n");
        let twice = upsert_agents_section(&once);
        assert_eq!(once, twice);
        assert!(once.starts_with("# Project\n\nNotes.\n\n<!-- gse:start"));
    }

    #[test]
    fn agents_section_replaces_in_place() {
        let old = format!("A\n{AGENTS_START}\nold\n{AGENTS_END}\nB\n");
        let new = upsert_agents_section(&old);
        assert!(new.starts_with("A\n<!-- gse:start"));
        assert!(new.ends_with(&format!("{AGENTS_END}\nB\n")));
        assert!(!new.contains("\nold\n"));
    }
}
