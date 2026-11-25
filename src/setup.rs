//! `gse keygen`, `init`, `track`, `untrack`, `commit`, and `decrypt`.

use crate::access::render_personal;
use crate::config::{self, FileRule, GlobalConfig, RepoConfig, USER_SCOPE, compile_glob};
use crate::git::Repo;
use crate::recipients::{Owner, OwnerKind, RecipientList};
use crate::sync::{Ctx, Scope, personal_recipients, walk_age};
use crate::templates::{AGENTS_START, GITATTRIBUTES, GSE_README, upsert_agents_section};
use crate::util::{write_file, write_private};
use crate::{github, identity, ui};
use anyhow::{Context, Result, bail};
use std::io::Read;
use std::path::Path;

pub struct KeygenArgs {
    pub handle: Option<String>,
    pub github: Option<String>,
    pub import: Option<String>,
    pub force: bool,
}

fn default_handle() -> String {
    let raw = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_default()
        .to_lowercase();
    let cleaned: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches(|c| c == '-' || c == '.').to_string();
    if config::valid_name(&cleaned) {
        cleaned
    } else {
        "me".into()
    }
}

pub fn keygen(args: KeygenArgs) -> Result<()> {
    use age::secrecy::ExposeSecret;
    let id_path = config::identity_path()?;
    let existing = identity::load_own()?;
    let existing_cfg = GlobalConfig::load()?;

    let (id, fresh) = match (&args.import, existing) {
        (Some(src), old) => {
            let text = if src == "-" {
                let mut s = String::new();
                std::io::stdin().read_to_string(&mut s)?;
                s
            } else {
                std::fs::read_to_string(src).with_context(|| format!("cannot read {src}"))?
            };
            let id = identity::parse_secret(&text)?;
            if let Some(old) = old
                && old.to_public().to_string() != id.to_public().to_string()
                && !args.force
            {
                bail!(
                    "{} already holds a different identity; pass --force to replace it \
                     (files encrypted only to the old key become unreadable)",
                    id_path.display()
                );
            }
            (id, true)
        }
        (None, Some(_)) if args.force => (age::x25519::Identity::generate(), true),
        (None, Some(old)) => (old, false),
        (None, None) => (age::x25519::Identity::generate(), true),
    };
    let public = id.to_public().to_string();

    let handle = match (&args.handle, &existing_cfg) {
        (Some(h), _) => h.clone(),
        (None, Some(cfg)) => cfg.handle.clone(),
        (None, None) => ui::prompt(
            "gse handle (your directory name in .gse/users/)",
            &default_handle(),
        ),
    };
    if !config::valid_name(&handle) {
        bail!("invalid handle {handle:?}: use lowercase letters, digits, `.`, `_`, and `-`");
    }
    let github_user = args
        .github
        .clone()
        .or_else(|| existing_cfg.as_ref().and_then(|c| c.github.clone()));

    if fresh {
        let text = format!(
            "# gse identity. Keep it secret, and back it up in a password manager.\n\
             # created: {}\n# public key: {public}\n{}\n",
            crate::util::today(),
            id.to_string().expose_secret()
        );
        write_private(
            &id_path,
            text.as_bytes(),
            &config::config_dir()?.join("tmp"),
        )?;
    }

    let mut list = if fresh {
        RecipientList::default()
    } else {
        personal_recipients(&handle).unwrap_or_default()
    };
    list.owners
        .retain(|o| !(o.kind == OwnerKind::Key && (o.keys.contains(&public) || o.name == handle)));
    let mut own = Owner::new(OwnerKind::Key, &handle);
    own.keys.push(public.clone());
    list.owners.insert(0, own);
    if let Some(user) = &github_user {
        match github::owner(user) {
            Ok(owner) => list.upsert(owner),
            Err(e) => ui::warn(format!("cannot fetch {user}'s GitHub keys: {e:#}")),
        }
    }
    write_file(
        &config::personal_recipients_path()?,
        render_personal(&list).as_bytes(),
    )?;
    GlobalConfig {
        handle: handle.clone(),
        github: github_user,
    }
    .save()?;

    if fresh {
        ui::info(format!("created your identity in {}", id_path.display()));
    } else {
        ui::info(format!("kept your identity in {}", id_path.display()));
    }
    ui::info(format!("handle: {handle}; public key below"));
    println!("{public}");
    Ok(())
}

pub struct InitArgs {
    pub no_track: bool,
    pub no_agents: bool,
}

const NOT_SECRETS: [&str; 8] = [
    ".env.example",
    ".env.sample",
    ".env.template",
    ".env.defaults",
    ".env.dist",
    ".env.vault",
    ".env.keys",
    ".envrc",
];

/// Untracked `.env*` files that look like real environments.
fn find_env_files(repo: &Repo) -> Result<Vec<String>> {
    let mut found: Vec<String> = repo
        .untracked()?
        .into_iter()
        .filter(|p| !p.ends_with('/') && !p.starts_with(".gse/"))
        .filter(|p| {
            let name = p.rsplit('/').next().unwrap_or(p);
            name.starts_with(".env")
                && !NOT_SECRETS.contains(&name)
                && ![".example", ".sample", ".template"]
                    .iter()
                    .any(|s| name.ends_with(s))
        })
        .collect();
    found.sort();
    Ok(found)
}

/// The `.gitignore` line covering a rule's pattern.
fn ignore_line(pattern: &str) -> String {
    match pattern.strip_prefix("**/") {
        Some(rest) if !rest.contains('/') => rest.to_string(),
        Some(_) => pattern.to_string(),
        None => format!("/{pattern}"),
    }
}

/// Adds `.gitignore` lines for tracked patterns git does not ignore yet.
/// Returns true when it changed `.gitignore`.
fn ensure_ignored(repo: &Repo, cfg: &RepoConfig) -> Result<bool> {
    let path = repo.root.join(".gitignore");
    let mut text = std::fs::read_to_string(&path).unwrap_or_default();
    let mut added = Vec::new();
    for rule in &cfg.files {
        let line = ignore_line(&rule.path);
        let sample = rule.path.strip_prefix("**/").unwrap_or(&rule.path);
        let literal = !sample.contains(['*', '?', '[', '{']);
        let covered = if literal {
            repo.is_ignored(sample)
        } else {
            text.lines().any(|l| l.trim() == line)
        };
        if !covered && !added.contains(&line) {
            added.push(line);
        }
    }
    if added.is_empty() {
        return Ok(false);
    }
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    if !text.contains("# Env files gse encrypts into .gse/") {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str("# Env files gse encrypts into .gse/\n");
    }
    for line in &added {
        text.push_str(line);
        text.push('\n');
    }
    write_file(&path, text.as_bytes())?;
    ui::info(format!("added to .gitignore: {}", added.join(", ")));
    Ok(true)
}

fn write_managed(root: &Path, rel: &str, content: &str) -> Result<()> {
    let path = root.join(rel);
    if std::fs::read_to_string(&path).ok().as_deref() != Some(content) {
        write_file(&path, content.as_bytes())?;
    }
    Ok(())
}

/// Writes the managed section into AGENTS.md, or CLAUDE.md when that is the
/// only agent file. Returns the file it changed.
fn write_agents(root: &Path) -> Result<Option<String>> {
    let read = |name: &str| std::fs::read_to_string(root.join(name)).ok();
    let agents = read("AGENTS.md");
    let claude = read("CLAUDE.md");
    let target = match (&agents, &claude) {
        (_, Some(c)) if c.contains(AGENTS_START) => "CLAUDE.md",
        (Some(_), _) => "AGENTS.md",
        (None, Some(_)) => "CLAUDE.md",
        (None, None) => "AGENTS.md",
    };
    let existing = read(target).unwrap_or_default();
    let updated = upsert_agents_section(&existing);
    if updated == existing {
        return Ok(None);
    }
    write_file(&root.join(target), updated.as_bytes())?;
    Ok(Some(target.to_string()))
}

fn open_ctx(repo: &Repo) -> Result<Ctx> {
    Ctx::open(Repo::at(&repo.root))?
        .context("this repository has no .gse/config.toml; run `gse init`")
}

pub fn init(args: InitArgs) -> Result<()> {
    let repo = Repo::discover()?.context("run `gse init` inside a git repository")?;
    let root = repo.root.clone();
    let existed = RepoConfig::path(&root).exists();
    let mut cfg = RepoConfig::load(&root)?.unwrap_or_default();
    if !existed {
        if !args.no_track {
            let found = find_env_files(&repo)?;
            if found.is_empty() {
                cfg.files.push(FileRule {
                    path: "**/.env.local".into(),
                    scope: USER_SCOPE.into(),
                });
                ui::info("tracking **/.env.local; `gse track <pattern>` adds more");
            } else {
                for path in &found {
                    cfg.files.push(FileRule {
                        path: path.clone(),
                        scope: USER_SCOPE.into(),
                    });
                }
                ui::info(format!("tracking {}", found.join(", ")));
            }
        }
        cfg.save(&root)?;
    }
    write_managed(&root, ".gse/.gitattributes", GITATTRIBUTES)?;
    write_managed(&root, ".gse/README.md", GSE_README)?;
    let mut stage = vec![
        ".gse/config.toml".to_string(),
        ".gse/.gitattributes".to_string(),
        ".gse/README.md".to_string(),
    ];
    if ensure_ignored(&repo, &cfg)? {
        stage.push(".gitignore".into());
    }
    if !args.no_agents
        && let Some(file) = write_agents(&root)?
    {
        ui::info(format!("wrote the gse section in {file}"));
        stage.push(file);
    }
    repo.add(&stage)?;
    let ctx = open_ctx(&repo)?;
    if ctx.me.is_none() {
        ui::warn("no gse identity on this machine; run `gse keygen`, then `gse sync`");
    }
    ctx.sync()?;
    ui::info("staged the changes; review them with `git status` and commit");
    Ok(())
}

pub fn track(pattern: &str, scope: &str) -> Result<()> {
    let repo = Repo::discover()?.context("run `gse track` inside a git repository")?;
    let mut cfg = RepoConfig::load(&repo.root)?.context("run `gse init` first")?;
    if scope != USER_SCOPE && !config::valid_name(scope) {
        bail!(
            "invalid scope {scope:?}: use `user` or lowercase letters, digits, `.`, `_`, and `-`"
        );
    }
    if pattern.starts_with(".gse/") || pattern.starts_with("/") {
        bail!("patterns are relative to the repository root and outside .gse/");
    }
    compile_glob(pattern)?;
    match cfg.files.iter_mut().find(|r| r.path == pattern) {
        Some(rule) => rule.scope = scope.to_string(),
        None => cfg.files.push(FileRule {
            path: pattern.to_string(),
            scope: scope.to_string(),
        }),
    }
    cfg.save(&repo.root)?;
    let glob = compile_glob(pattern)?;
    let moved = stale_copies(&repo, &cfg, &glob)?;
    repo.rm(&moved)?;
    let mut stage = vec![".gse/config.toml".to_string()];
    if ensure_ignored(&repo, &cfg)? {
        stage.push(".gitignore".into());
    }
    repo.add(&stage)?;
    for path in repo.indexed()? {
        if glob.is_match(&path) {
            ui::warn(format!(
                "{path} is committed in plaintext; remove it with `git rm --cached {path}` \
                 (and rotate anything in its history)"
            ));
        }
    }
    warn_shadowed(&repo, &cfg, pattern, &glob, scope)?;
    ui::info(format!("tracking {pattern} in scope {scope}"));
    let ctx = open_ctx(&repo)?;
    for cipher in &moved {
        ctx.state.clear_base(cipher);
    }
    if !moved.is_empty() {
        ui::info(format!(
            "removed {} encrypted cop{} from the previous scope",
            moved.len(),
            if moved.len() == 1 { "y" } else { "ies" }
        ));
    }
    ctx.sync()?;
    Ok(())
}

/// Encrypted copies of files matching `glob` in a scope the config no longer
/// assigns them to.
fn stale_copies(repo: &Repo, cfg: &RepoConfig, glob: &globset::GlobMatcher) -> Result<Vec<String>> {
    let matcher = cfg.matcher()?;
    let mut stale = Vec::new();
    for dir in [".gse/users", ".gse/shared"] {
        for cipher in walk_age(&repo.root, dir) {
            let Some(e) = Ctx::entry_for_cipher(&cipher) else {
                continue;
            };
            if !glob.is_match(&e.plain) {
                continue;
            }
            let current = match (matcher.scope_for(&e.plain), &e.scope) {
                (Some(USER_SCOPE), Scope::User(_)) => true,
                (Some(want), Scope::Shared(have)) => want == have,
                _ => false,
            };
            if !current {
                stale.push(cipher);
            }
        }
    }
    Ok(stale)
}

/// Warns about files `pattern` matches that an earlier rule assigns to
/// another scope, since the first matching rule wins.
fn warn_shadowed(
    repo: &Repo,
    cfg: &RepoConfig,
    pattern: &str,
    glob: &globset::GlobMatcher,
    scope: &str,
) -> Result<()> {
    let mut paths: Vec<String> = repo
        .candidates()?
        .into_iter()
        .filter(|p| !p.ends_with('/') && !p.starts_with(".gse/") && glob.is_match(p))
        .collect();
    if !pattern.contains(['*', '?', '[', '{']) && !paths.iter().any(|p| p == pattern) {
        paths.push(pattern.to_string());
    }
    for path in paths {
        let winner = cfg
            .files
            .iter()
            .find(|r| compile_glob(&r.path).is_ok_and(|g| g.is_match(&path)));
        if let Some(rule) = winner
            && rule.scope != scope
        {
            ui::warn(format!(
                "{path} stays in scope {} because the earlier rule `{}` matches it first; \
                 untrack that rule or move it below this one in .gse/config.toml",
                rule.scope, rule.path
            ));
        }
    }
    Ok(())
}

pub fn untrack(pattern: &str) -> Result<()> {
    let repo = Repo::discover()?.context("run `gse untrack` inside a git repository")?;
    let mut cfg = RepoConfig::load(&repo.root)?.context("run `gse init` first")?;
    let before = cfg.files.len();
    cfg.files.retain(|r| r.path != pattern);
    if cfg.files.len() == before {
        bail!("no tracked pattern {pattern:?} in .gse/config.toml");
    }
    cfg.save(&repo.root)?;
    let gone = stale_copies(&repo, &cfg, &compile_glob(pattern)?)?;
    repo.rm(&gone)?;
    repo.add(&[".gse/config.toml".to_string()])?;
    let ctx = open_ctx(&repo)?;
    for cipher in &gone {
        ctx.state.clear_base(cipher);
    }
    ui::info(format!(
        "untracked {pattern} and removed {} encrypted cop{}; plaintext files stay as they are",
        gone.len(),
        if gone.len() == 1 { "y" } else { "ies" }
    ));
    Ok(())
}

pub fn sync() -> Result<i32> {
    let repo = Repo::discover()?.context("run `gse sync` inside a git repository")?;
    let ctx = open_ctx(&repo)?;
    Ok(if ctx.sync()? { 0 } else { 1 })
}

pub fn status() -> Result<()> {
    let repo = Repo::discover()?.context("run `gse status` inside a git repository")?;
    open_ctx(&repo)?.status()
}

pub fn commit(message: Option<String>) -> Result<i32> {
    let repo = Repo::discover()?.context("run `gse commit` inside a git repository")?;
    {
        // The commit's own pre-commit hook takes the lock, so release it first.
        let ctx = open_ctx(&repo)?;
        if !ctx.sync()? {
            bail!("resolve the problems above, then run `gse commit` again");
        }
    }
    let pending = repo.run(&["status", "--porcelain", "--", ".gse"])?;
    if pending.trim().is_empty() {
        ui::info("nothing to commit in .gse/");
        return Ok(0);
    }
    let message = message.unwrap_or_else(|| "Update encrypted env files".into());
    let status = repo
        .cmd()
        .args(["commit", "-m", &message, "--", ".gse"])
        .status()
        .context("cannot run git commit")?;
    Ok(status.code().unwrap_or(1))
}

pub fn decrypt(scope: Option<String>, force: bool) -> Result<()> {
    let repo = Repo::discover()?.context("run `gse decrypt` inside a git repository")?;
    let ctx = open_ctx(&repo)?;
    let mut wrote = 0;
    for e in ctx.entries()? {
        let wanted = match (&scope, &e.scope) {
            (None, _) => true,
            (Some(s), Scope::User(_)) => s == USER_SCOPE,
            (Some(s), Scope::Shared(name)) => s == name,
        };
        if !wanted {
            continue;
        }
        if ctx.abs(&e.plain).exists() && !force {
            ui::info(format!("{} exists; --force overwrites it", e.plain));
            continue;
        }
        let Ok(cipher) = std::fs::read(ctx.abs(&e.cipher)) else {
            continue;
        };
        let Some(d) = ctx.decrypt(&cipher)? else {
            continue;
        };
        ctx.write_plain(&e, &d.content)?;
        ctx.state.set_base(&e.cipher, &cipher)?;
        ui::info(format!("decrypted {}", e.plain));
        wrote += 1;
    }
    if wrote == 0 {
        ui::info("decrypted nothing: no readable files were missing");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignore_lines() {
        assert_eq!(ignore_line("**/.env.local"), ".env.local");
        assert_eq!(ignore_line(".env"), "/.env");
        assert_eq!(ignore_line("apps/web/.env.local"), "/apps/web/.env.local");
        assert_eq!(ignore_line("**/config/.env"), "**/config/.env");
    }
}
