//! `gse grant`, `gse revoke`, and `gse refresh`: change who can read a scope.
//!
//! Each change rewrites every file in the scope, which requires decrypting
//! them, so only a member can change a scope's recipients.

use crate::config::{self, GlobalConfig, USER_SCOPE};
use crate::crypto::{self, Decrypted};
use crate::recipients::{KeyCheck, Owner, OwnerKind, RecipientList, check_key, normalize_key};
use crate::sync::{Ctx, Scope, ScopeInfo, walk_age};
use crate::ui;
use crate::util::{read_opt, write_file};
use crate::{github, identity};
use anyhow::{Context, Result, bail};

pub struct GrantArgs {
    pub scope: String,
    pub users: Vec<String>,
    pub key: Option<String>,
    pub name: Option<String>,
    pub new_key: Option<String>,
}

fn check_scope(scope: &str) -> Result<()> {
    if scope == USER_SCOPE {
        bail!(
            "the user scope belongs to each developer; grant access to a shared scope such as `team`"
        );
    }
    if !config::valid_name(scope) {
        bail!("invalid scope name {scope:?}: use lowercase letters, digits, `.`, `_`, and `-`");
    }
    Ok(())
}

/// The scope's files, decrypted, and its current list.
fn load_scope(ctx: &Ctx, scope: &str) -> Result<(Vec<(String, Decrypted)>, RecipientList)> {
    let files = walk_age(&ctx.repo.root, &Scope::Shared(scope.to_string()).dir());
    let mut decrypted = Vec::new();
    for file in files {
        let bytes = read_opt(&ctx.abs(&file)).with_context(|| format!("cannot read {file}"))?;
        let d = ctx.decrypt(&bytes)?.with_context(|| {
            format!("your keys cannot open {file}, so you cannot change scope {scope}")
        })?;
        decrypted.push((file, d));
    }
    let list = match ctx.scope_info(scope) {
        ScopeInfo::Member(list) => list,
        // A scope's list lives inside its files, so a scope needs one first.
        ScopeInfo::Empty => bail!(
            "scope {scope} has no files yet. Track one into it first, for example \
             `gse track .env.{scope} --scope {scope}`, then run this again."
        ),
        ScopeInfo::Outsider => bail!("your keys cannot open scope {scope}"),
    };
    Ok((decrypted, list))
}

/// Re-encrypts every file in the scope to `list` and stages the result.
fn rewrite(
    ctx: &Ctx,
    scope: &str,
    files: &[(String, Decrypted)],
    list: &RecipientList,
) -> Result<()> {
    if list.is_empty() {
        bail!("refusing to leave scope {scope} with no recipients");
    }
    let mut staged = Vec::new();
    for (file, d) in files {
        let path = ctx.abs(file);
        let old = read_opt(&path);
        let new = crypto::encrypt(list, &d.content)?;
        write_file(&path, &new)?;
        if old.is_some() && ctx.state.base(file) == old {
            ctx.state.set_base(file, &new)?;
        }
        staged.push(file.clone());
    }
    ctx.repo.add(&staged)?;
    ctx.forget_scope(scope);
    ctx.write_mirror(&Scope::Shared(scope.to_string()), list)?;
    Ok(())
}

pub fn grant(ctx: &Ctx, args: GrantArgs) -> Result<()> {
    check_scope(&args.scope)?;
    if args.users.is_empty() && args.key.is_none() && args.new_key.is_none() {
        bail!("name GitHub users, or pass --key or --new-key");
    }
    ctx.pull(true)?;
    let (files, mut list) = load_scope(ctx, &args.scope)?;
    let before = list.clone();
    for user in &args.users {
        let owner = github::owner(user)?;
        if owner.keys.is_empty() {
            bail!(
                "{user} has no ssh-ed25519 or ssh-rsa keys on GitHub; add a gse key for them with \
                 `gse grant --key <age1...> --name {user} --scope {}`",
                args.scope
            );
        }
        if !owner.unsupported.is_empty() {
            ui::warn(format!(
                "{user}: {} GitHub key(s) are types age cannot use; recorded as comments",
                owner.unsupported.len()
            ));
        }
        list.upsert(owner);
    }
    if let Some(key) = &args.key {
        let name = args
            .name
            .as_deref()
            .context("--key needs --name to label the key's owner")?;
        if !config::valid_owner_name(name) {
            bail!("invalid name {name:?}");
        }
        let key = normalize_key(key);
        match check_key(&key) {
            KeyCheck::Supported => {}
            KeyCheck::Unsupported(why) | KeyCheck::Invalid(why) => {
                bail!("cannot use that key: {why}")
            }
        }
        let mut owner = list
            .owners
            .iter()
            .find(|o| o.kind == OwnerKind::Key && o.name == name)
            .cloned()
            .unwrap_or_else(|| Owner::new(OwnerKind::Key, name));
        owner.note = Some(format!("added {}", crate::util::today()));
        if !owner.keys.contains(&key) {
            owner.keys.push(key);
        }
        list.upsert(owner);
    }
    let mut secret = None;
    if let Some(name) = &args.new_key {
        if !config::valid_owner_name(name) {
            bail!("invalid name {name:?}");
        }
        let id = age::x25519::Identity::generate();
        let mut owner = Owner::new(OwnerKind::Key, name);
        owner.note = Some(format!("added {}", crate::util::today()));
        owner.keys.push(id.to_public().to_string());
        list.upsert(owner);
        secret = Some(id);
    }
    rewrite(ctx, &args.scope, &files, &list)?;
    for line in before.changes(&list) {
        ui::info(format!("scope {}: {line}", args.scope));
    }
    ui::info("staged; commit to publish the change");
    if let Some(id) = secret {
        use age::secrecy::ExposeSecret;
        ui::info(format!(
            "secret key for {} below; store it now (for example as the GSE_IDENTITY secret in CI). \
             gse keeps no copy.",
            args.new_key.as_deref().unwrap_or_default()
        ));
        println!("{}", id.to_string().expose_secret());
    }
    Ok(())
}

pub fn revoke(ctx: &Ctx, scope: &str, name: &str) -> Result<()> {
    check_scope(scope)?;
    ctx.pull(true)?;
    let (files, mut list) = load_scope(ctx, scope)?;
    let before = list.clone();
    if list.remove(name) == 0 {
        bail!("no owner named {name} in scope {scope}");
    }
    let mine = ctx
        .me
        .as_ref()
        .map(|m| m.personal.key_set())
        .unwrap_or_default();
    if !mine.is_empty() && list.key_set().is_disjoint(&mine) {
        ui::warn(format!("this removes your own access to scope {scope}"));
    }
    rewrite(ctx, scope, &files, &list)?;
    for line in before.changes(&list) {
        ui::info(format!("scope {scope}: {line}"));
    }
    ui::warn(format!(
        "{name} can still read every version already in git history: rotate the secrets in scope {scope}"
    ));
    ui::info("staged; commit to publish the change");
    Ok(())
}

/// Re-fetches every `@github` owner in a scope's list. Returns true when the
/// list changed.
pub fn refresh_scope(ctx: &Ctx, scope: &str) -> Result<bool> {
    check_scope(scope)?;
    let (files, mut list) = load_scope(ctx, scope)?;
    let before = list.clone();
    let github: Vec<String> = list
        .owners
        .iter()
        .filter(|o| o.kind == OwnerKind::Github)
        .map(|o| o.name.clone())
        .collect();
    for user in github {
        let fresh = github::owner(&user)?;
        if fresh.keys.is_empty() {
            ui::warn(format!(
                "{user} has no usable keys on GitHub now; keeping the old ones"
            ));
            continue;
        }
        list.upsert(fresh);
    }
    let changes = before.changes(&list);
    if changes.is_empty() {
        ui::info(format!("scope {scope}: no key changes"));
        return Ok(false);
    }
    rewrite(ctx, scope, &files, &list)?;
    for line in &changes {
        ui::info(format!("scope {scope}: {line}"));
    }
    if changes.iter().any(|c| c.starts_with('-')) {
        ui::warn(format!(
            "removed keys can still read every version already in git history: rotate the secrets in scope {scope}"
        ));
    }
    Ok(true)
}

/// Re-fetches your own GitHub keys into `~/.config/gse/recipients.txt`.
pub fn refresh_me() -> Result<()> {
    let global =
        GlobalConfig::load()?.context("no gse identity on this machine; run `gse keygen`")?;
    let path = config::personal_recipients_path()?;
    let mut list = crate::sync::personal_recipients(&global.handle)?;
    let before = list.clone();
    if let Some(id) = identity::load_own()? {
        let key = id.to_public().to_string();
        if !list.key_set().contains(&key) {
            let mut owner = Owner::new(OwnerKind::Key, &global.handle);
            owner.keys.push(key);
            list.upsert(owner);
        }
    }
    match &global.github {
        Some(user) => list.upsert(github::owner(user)?),
        None => ui::info(
            "no GitHub username in ~/.config/gse/config.toml; `gse keygen --github <user>` adds one",
        ),
    }
    write_file(&path, render_personal(&list).as_bytes())?;
    for line in before.changes(&list) {
        ui::info(format!("personal: {line}"));
    }
    Ok(())
}

pub fn render_personal(list: &RecipientList) -> String {
    list.render(&[
        "Personal recipients: gse encrypts your files in .gse/users/<handle>/ to these keys.",
        "`gse keygen` and `gse refresh --me` write this file.",
    ])
}
