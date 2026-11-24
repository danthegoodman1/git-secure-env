//! Sync between plaintext env files and their encrypted copies in `.gse/`.
//!
//! Outbound (pre-commit, `gse sync`) encrypts a plaintext file when its content
//! changed. Inbound (post-checkout, post-merge, post-rewrite, `gse sync`)
//! decrypts a ciphertext that changed, merging with local edits against the
//! last-synced ciphertext kept in `.git/gse/base/`.

use crate::config::{self, GlobalConfig, Matcher, RepoConfig, USER_SCOPE};
use crate::crypto::{self, Decrypted};
use crate::git::Repo;
use crate::identity::{self, Identities};
use crate::merge;
use crate::recipients::{Owner, OwnerKind, RecipientList};
use crate::state::State;
use crate::ui;
use crate::util::{has_conflict_markers, read_opt, write_file, write_private};
use anyhow::{Context, Result};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scope {
    User(String),
    Shared(String),
}

impl Scope {
    pub fn dir(&self) -> String {
        match self {
            Scope::User(h) => format!(".gse/users/{h}"),
            Scope::Shared(s) => format!(".gse/shared/{s}"),
        }
    }

    pub fn label(&self) -> String {
        match self {
            Scope::User(h) => format!("users/{h}"),
            Scope::Shared(s) => format!("shared/{s}"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub plain: String,
    pub scope: Scope,
    pub cipher: String,
}

impl Entry {
    pub fn new(plain: &str, scope: Scope) -> Self {
        let cipher = format!("{}/{plain}.age", scope.dir());
        Self {
            plain: plain.to_string(),
            scope,
            cipher,
        }
    }
}

pub struct Me {
    pub handle: String,
    pub personal: RecipientList,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outbound {
    Missing,
    Conflicted,
    InSync,
    Encrypted,
    NotRecipient,
    Collision,
    NoRecipients,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Inbound {
    NoCipher,
    InSync,
    Restored,
    Updated,
    Merged,
    Conflict,
    NotRecipient,
}

#[derive(Clone)]
pub enum ScopeInfo {
    Member(RecipientList),
    Empty,
    Outsider,
}

pub struct Ctx {
    pub repo: Repo,
    pub cfg: RepoConfig,
    pub matcher: Matcher,
    pub me: Option<Me>,
    pub ids: Identities,
    pub state: State,
    scopes: RefCell<HashMap<String, ScopeInfo>>,
}

/// The personal recipient list: `~/.config/gse/recipients.txt`, or the gse
/// identity's public key when that file is missing.
pub fn personal_recipients(handle: &str) -> Result<RecipientList> {
    let path = config::personal_recipients_path()?;
    if let Ok(text) = std::fs::read_to_string(&path) {
        return RecipientList::parse(&text)
            .with_context(|| format!("cannot parse {}", path.display()));
    }
    let mut list = RecipientList::default();
    if let Some(id) = identity::load_own()? {
        let mut owner = Owner::new(OwnerKind::Key, handle);
        owner.keys.push(id.to_public().to_string());
        list.upsert(owner);
    }
    Ok(list)
}

pub fn load_me() -> Result<Option<Me>> {
    let Some(global) = GlobalConfig::load()? else {
        return Ok(None);
    };
    let personal = personal_recipients(&global.handle)?;
    if personal.is_empty() {
        return Ok(None);
    }
    Ok(Some(Me {
        handle: global.handle,
        personal,
    }))
}

/// Repository-relative paths of the `.age` files under `rel`, sorted.
pub fn walk_age(root: &Path, rel: &str) -> Vec<String> {
    fn walk(root: &Path, rel: &str, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(root.join(rel)) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let child = format!("{rel}/{name}");
            match entry.file_type() {
                Ok(t) if t.is_dir() => walk(root, &child, out),
                Ok(_) if name.ends_with(".age") => out.push(child),
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    walk(root, rel, &mut out);
    out.sort();
    out
}

impl Ctx {
    /// Opens the repository's gse context, or `None` without `.gse/config.toml`.
    pub fn open(repo: Repo) -> Result<Option<Ctx>> {
        let Some(cfg) = RepoConfig::load(&repo.root)? else {
            return Ok(None);
        };
        let matcher = cfg.matcher()?;
        let me = load_me()?;
        let ids = Identities::load()?;
        let state = State::open(&repo)?;
        Ok(Some(Ctx {
            repo,
            cfg,
            matcher,
            me,
            ids,
            state,
            scopes: RefCell::new(HashMap::new()),
        }))
    }

    pub fn abs(&self, rel: &str) -> PathBuf {
        self.repo.root.join(rel)
    }

    pub fn decrypt(&self, bytes: &[u8]) -> Result<Option<Decrypted>> {
        crypto::decrypt(bytes, &self.ids)
    }

    pub fn entry_for_plain(&self, plain: &str) -> Option<Entry> {
        let scope = self.matcher.scope_for(plain)?;
        let scope = if scope == USER_SCOPE {
            Scope::User(self.me.as_ref()?.handle.clone())
        } else {
            Scope::Shared(scope.to_string())
        };
        Some(Entry::new(plain, scope))
    }

    pub fn entry_for_cipher(cipher: &str) -> Option<Entry> {
        let rest = cipher.strip_prefix(".gse/")?;
        let (scope, tail) = if let Some(r) = rest.strip_prefix("users/") {
            let (h, t) = r.split_once('/')?;
            (Scope::User(h.to_string()), t)
        } else {
            let (s, t) = rest.strip_prefix("shared/")?.split_once('/')?;
            (Scope::Shared(s.to_string()), t)
        };
        let plain = tail.strip_suffix(".age")?;
        Some(Entry {
            plain: plain.to_string(),
            scope,
            cipher: cipher.to_string(),
        })
    }

    pub fn shared_scopes(&self) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(self.abs(".gse/shared")) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .flatten()
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| config::valid_name(n))
            .collect();
        names.sort();
        names
    }

    /// Every file gse syncs here: plaintext files matching a rule, plus
    /// ciphertexts in your user directory and in each shared scope.
    pub fn entries(&self) -> Result<Vec<Entry>> {
        let mut map: BTreeMap<String, Entry> = BTreeMap::new();
        for path in self.repo.candidates()? {
            if path.ends_with('/') || path.starts_with(".gse/") {
                continue;
            }
            if let Some(entry) = self.entry_for_plain(&path) {
                map.entry(entry.cipher.clone()).or_insert(entry);
            }
        }
        let mut dirs = Vec::new();
        if let Some(me) = &self.me {
            dirs.push(Scope::User(me.handle.clone()).dir());
        }
        for scope in self.shared_scopes() {
            dirs.push(Scope::Shared(scope).dir());
        }
        for dir in dirs {
            for cipher in walk_age(&self.repo.root, &dir) {
                if let Some(entry) = Self::entry_for_cipher(&cipher) {
                    map.entry(cipher).or_insert(entry);
                }
            }
        }
        Ok(map.into_values().collect())
    }

    /// Whether this machine can read a shared scope, and its recipient list.
    pub fn scope_info(&self, name: &str) -> ScopeInfo {
        if let Some(info) = self.scopes.borrow().get(name) {
            return info.clone();
        }
        let files = walk_age(&self.repo.root, &Scope::Shared(name.to_string()).dir());
        let mut info = if files.is_empty() {
            ScopeInfo::Empty
        } else {
            ScopeInfo::Outsider
        };
        for file in &files {
            let Some(bytes) = read_opt(&self.abs(file)) else {
                continue;
            };
            if let Ok(Some(d)) = self.decrypt(&bytes)
                && !d.list.is_empty()
            {
                info = ScopeInfo::Member(d.list);
                break;
            }
        }
        self.scopes
            .borrow_mut()
            .insert(name.to_string(), info.clone());
        info
    }

    pub fn forget_scope(&self, name: &str) {
        self.scopes.borrow_mut().remove(name);
    }

    /// The list a file encrypts to: your personal list in your user directory;
    /// in a shared scope, the list inside the file's current ciphertext, else
    /// the scope's, else (a new scope) your own keys.
    fn target_list(&self, e: &Entry, current: Option<&Decrypted>) -> Option<RecipientList> {
        match &e.scope {
            Scope::User(_) => self.me.as_ref().map(|m| m.personal.clone()),
            Scope::Shared(name) => {
                if let Some(d) = current
                    && !d.list.is_empty()
                {
                    return Some(d.list.clone());
                }
                match self.scope_info(name) {
                    ScopeInfo::Member(list) => Some(list),
                    ScopeInfo::Empty => self.me.as_ref().map(|m| m.personal.clone()),
                    ScopeInfo::Outsider => None,
                }
            }
        }
    }

    pub fn write_plain(&self, e: &Entry, bytes: &[u8]) -> Result<()> {
        write_private(&self.abs(&e.plain), bytes, &self.state.tmp_dir())
    }

    /// Writes a scope's `recipients.txt` mirror when it differs and stages it.
    pub fn write_mirror(&self, scope: &Scope, list: &RecipientList) -> Result<()> {
        let rel = format!("{}/recipients.txt", scope.dir());
        let header: Vec<String> = match scope {
            Scope::User(h) => vec![
                format!("Keys that can read .gse/users/{h}/ (from that person's ~/.config/gse/recipients.txt)."),
                "A valid age recipients file. gse rewrites it; edits here change nothing gse encrypts to.".into(),
            ],
            Scope::Shared(s) => vec![
                format!("Keys that can read .gse/shared/{s}/. Change them with gse grant, revoke, and refresh."),
                "A valid age recipients file. gse rewrites it; edits here change nothing gse encrypts to.".into(),
            ],
        };
        let header: Vec<&str> = header.iter().map(String::as_str).collect();
        let text = list.render(&header);
        let path = self.abs(&rel);
        let existing = std::fs::read_to_string(&path).ok();
        if existing.as_deref() == Some(text.as_str()) {
            return Ok(());
        }
        if let (Scope::Shared(_), Some(old)) = (scope, &existing)
            && let Ok(old) = RecipientList::parse(old)
            && old.key_set() != list.key_set()
        {
            ui::warn(format!(
                "{rel} differed from the scope's encrypted recipient list; gse restored it"
            ));
        }
        write_file(&path, text.as_bytes())?;
        self.repo.add(&[rel])
    }

    pub fn outbound(&self, e: &Entry) -> Result<Outbound> {
        let plain_path = self.abs(&e.plain);
        let Some(mut plain) = read_opt(&plain_path) else {
            return Ok(Outbound::Missing);
        };
        if has_conflict_markers(&plain) {
            self.state.set_conflict(&e.plain, true)?;
            return Ok(Outbound::Conflicted);
        }
        self.state.set_conflict(&e.plain, false)?;
        let cipher_path = self.abs(&e.cipher);
        let mut cipher = read_opt(&cipher_path);
        if let Some(c) = &cipher
            && self.state.base(&e.cipher).as_deref() != Some(c.as_slice())
        {
            if self.inbound(e)? == Inbound::Conflict {
                return Ok(Outbound::Conflicted);
            }
            plain = read_opt(&plain_path).unwrap_or_default();
            cipher = read_opt(&cipher_path);
        }
        let current = match &cipher {
            Some(c) => match self.decrypt(c)? {
                Some(d) => Some(d),
                None => {
                    return Ok(match e.scope {
                        Scope::User(_) => Outbound::Collision,
                        Scope::Shared(_) => Outbound::NotRecipient,
                    });
                }
            },
            None => None,
        };
        let Some(target) = self.target_list(e, current.as_ref()) else {
            return Ok(Outbound::NotRecipient);
        };
        if target.is_empty() {
            return Ok(Outbound::NoRecipients);
        }
        if let (Some(d), Some(c)) = (&current, &cipher) {
            let same_content = d.content == plain;
            let user = matches!(e.scope, Scope::User(_));
            let missing_keys = user && !target.key_set().is_subset(&d.list.key_set());
            if same_content && !missing_keys {
                self.state.set_base(&e.cipher, c)?;
                self.write_mirror(&e.scope, &target)?;
                return Ok(Outbound::InSync);
            }
            if user && !d.list.key_set().is_subset(&target.key_set()) {
                ui::warn(format!(
                    "{} was encrypted to keys missing from your ~/.config/gse/recipients.txt; \
                     re-encrypting drops them (run `gse refresh --me` if you added keys on another machine)",
                    e.cipher
                ));
            }
        }
        let new = crypto::encrypt(&target, &plain)?;
        write_file(&cipher_path, &new)?;
        self.repo.add(std::slice::from_ref(&e.cipher))?;
        self.state.set_base(&e.cipher, &new)?;
        self.write_mirror(&e.scope, &target)?;
        Ok(Outbound::Encrypted)
    }

    pub fn inbound(&self, e: &Entry) -> Result<Inbound> {
        let Some(cipher) = read_opt(&self.abs(&e.cipher)) else {
            return Ok(Inbound::NoCipher);
        };
        let plain = read_opt(&self.abs(&e.plain));
        let base = self.state.base(&e.cipher);
        if base.as_deref() == Some(cipher.as_slice()) && plain.is_some() {
            return Ok(Inbound::InSync);
        }
        let Some(new) = self.decrypt(&cipher)? else {
            return Ok(Inbound::NotRecipient);
        };
        let base_dec = base.as_ref().and_then(|b| self.decrypt(b).ok().flatten());
        if let Some(old) = &base_dec {
            let changes = old.list.changes(&new.list);
            if !changes.is_empty() {
                ui::warn(format!(
                    "recipients of {} changed in {}:\n  {}",
                    e.cipher,
                    self.repo.last_change(&e.cipher),
                    changes.join("\n  ")
                ));
            }
        }
        let result = match plain {
            None => {
                self.write_plain(e, &new.content)?;
                Inbound::Restored
            }
            Some(p) if p == new.content => Inbound::InSync,
            Some(p) if base_dec.as_ref().is_some_and(|b| b.content == p) => {
                self.write_plain(e, &new.content)?;
                Inbound::Updated
            }
            Some(p) => {
                let ancestor = base_dec.map(|b| b.content).unwrap_or_default();
                match merge::merge3(&ancestor, &p, &new.content, "local", "incoming") {
                    Ok(merged) => {
                        self.write_plain(e, &merged)?;
                        Inbound::Merged
                    }
                    Err(conflicted) => {
                        self.write_plain(e, &conflicted)?;
                        self.state.set_conflict(&e.plain, true)?;
                        Inbound::Conflict
                    }
                }
            }
        };
        self.state.set_base(&e.cipher, &cipher)?;
        Ok(result)
    }

    fn report_outbound(&self, e: &Entry, o: &Outbound) {
        match o {
            Outbound::Encrypted => {
                ui::info(format!("encrypted {} -> {}", e.plain, e.cipher));
                if matches!(e.scope, Scope::Shared(_)) {
                    self.warn_shared_personal_values(e);
                }
            }
            Outbound::Collision => ui::warn(format!(
                "{} belongs to someone else: your keys cannot open it. \
                 Pick another handle with `gse keygen --handle <name>`.",
                e.scope.dir()
            )),
            Outbound::NotRecipient => {
                if let Scope::Shared(s) = &e.scope {
                    ui::warn(format!(
                        "{} is in scope {s}, which you cannot read; ask a member to run `gse grant <you> --scope {s}`",
                        e.plain
                    ));
                }
            }
            Outbound::NoRecipients => {
                ui::warn(format!("{} has no recipients to encrypt to", e.plain))
            }
            Outbound::Conflicted => ui::warn(format!(
                "{} has conflict markers; resolve them, then run `gse sync`",
                e.plain
            )),
            Outbound::Missing | Outbound::InSync => {}
        }
    }

    fn report_inbound(&self, e: &Entry, r: &Inbound) {
        match r {
            Inbound::Restored => ui::info(format!("decrypted {}", e.plain)),
            Inbound::Updated => ui::info(format!("updated {}", e.plain)),
            Inbound::Merged => ui::info(format!("merged incoming changes into {}", e.plain)),
            Inbound::Conflict => ui::warn(format!(
                "{} conflicts with incoming changes; resolve the markers, then run `gse sync`",
                e.plain
            )),
            Inbound::NotRecipient if matches!(e.scope, Scope::User(_)) => ui::warn(format!(
                "{} belongs to someone else: your keys cannot open it",
                e.scope.dir()
            )),
            _ => {}
        }
    }

    /// The pre-commit hook. False blocks the commit.
    pub fn pre_commit(&self) -> Result<bool> {
        let leaks: Vec<String> = self
            .repo
            .indexed()?
            .into_iter()
            .filter(|p| !p.starts_with(".gse/") && self.matcher.scope_for(p).is_some())
            .collect();
        if !leaks.is_empty() {
            for p in &leaks {
                ui::error(format!(
                    "{p} is staged in plaintext. gse encrypts it into .gse/ instead; \
                     unstage it with `git rm --cached {p}`."
                ));
            }
            return Ok(false);
        }
        let mut ok = true;
        for e in self.entries()? {
            match self.outbound(&e) {
                Ok(Outbound::Conflicted) => {
                    ui::error(format!(
                        "{} has conflict markers; resolve them and run `gse sync` before committing",
                        e.plain
                    ));
                    ok = false;
                }
                Ok(o) => self.report_outbound(&e, &o),
                Err(err) => ui::warn(format!("{}: {err:#}", e.plain)),
            }
        }
        Ok(ok)
    }

    /// A `#personal` key with a value in a shared file publishes one
    /// developer's value to the whole scope.
    fn warn_shared_personal_values(&self, e: &Entry) {
        let Some(bytes) = read_opt(&self.abs(&e.plain)) else {
            return;
        };
        let keys: Vec<String> = crate::dotenv::parse(&String::from_utf8_lossy(&bytes))
            .into_iter()
            .filter(|v| v.personal && !v.value.is_empty())
            .map(|v| v.key)
            .collect();
        if !keys.is_empty() {
            ui::warn(format!(
                "{} is shared, but it gives #personal keys a value: {}. \
                 Move the values to a personal file such as .env.local.",
                e.plain,
                keys.join(", ")
            ));
        }
    }

    /// The post-commit hook. `git commit <path>` runs pre-commit against a
    /// temporary index, so ciphertext staged there reaches the commit but not
    /// the real index; staging it again keeps the index level with HEAD.
    pub fn post_commit(&self) -> Result<()> {
        let mut paths: Vec<String> = Vec::new();
        for e in self.entries()? {
            for path in [
                e.cipher.clone(),
                format!("{}/recipients.txt", e.scope.dir()),
            ] {
                if self.abs(&path).exists() && !paths.contains(&path) {
                    paths.push(path);
                }
            }
        }
        if paths.is_empty() {
            return Ok(());
        }
        let mut args = vec!["diff", "--name-only", "-z", "--"];
        args.extend(paths.iter().map(String::as_str));
        let changed: Vec<String> = self
            .repo
            .run(&args)?
            .split('\0')
            .filter(|p| !p.is_empty())
            .map(str::to_string)
            .collect();
        self.repo.add(&changed)
    }

    /// Inbound sync after pull, checkout, and rebase. Post-checkout passes
    /// `skip_mid_operation`, since it also fires partway through a rebase;
    /// post-merge runs while `MERGE_HEAD` still exists, so it passes false.
    pub fn pull(&self, skip_mid_operation: bool) -> Result<()> {
        if skip_mid_operation && self.repo.operation_in_progress() {
            return Ok(());
        }
        for e in self.entries()? {
            match self.inbound(&e) {
                Ok(r) => self.report_inbound(&e, &r),
                Err(err) => ui::warn(format!("{}: {err:#}", e.plain)),
            }
        }
        Ok(())
    }

    /// Inbound then outbound sync. Returns false when conflicts remain.
    pub fn sync(&self) -> Result<bool> {
        if self.me.is_none() {
            ui::warn("no gse identity on this machine; run `gse keygen` to sync personal files");
        }
        let entries = self.entries()?;
        let mut clean = true;
        if !self.repo.operation_in_progress() {
            for e in &entries {
                match self.inbound(e) {
                    Ok(r) => {
                        clean &= r != Inbound::Conflict;
                        self.report_inbound(e, &r);
                    }
                    Err(err) => {
                        ui::warn(format!("{}: {err:#}", e.plain));
                        clean = false;
                    }
                }
            }
        }
        for e in &entries {
            match self.outbound(e) {
                Ok(o) => {
                    clean &= o != Outbound::Conflicted;
                    self.report_outbound(e, &o);
                }
                Err(err) => {
                    ui::warn(format!("{}: {err:#}", e.plain));
                    clean = false;
                }
            }
        }
        Ok(clean)
    }

    pub fn describe(&self, e: &Entry) -> Result<String> {
        let plain = read_opt(&self.abs(&e.plain));
        let cipher = read_opt(&self.abs(&e.cipher));
        if plain.as_deref().is_some_and(has_conflict_markers) {
            return Ok("conflict: resolve the markers, then run `gse sync`".into());
        }
        let Some(c) = cipher else {
            return Ok(if plain.is_some() {
                "new: `gse sync` encrypts it".into()
            } else {
                "missing".into()
            });
        };
        let Some(d) = self.decrypt(&c)? else {
            return Ok(match &e.scope {
                Scope::User(_) => "unreadable: your keys cannot open it".into(),
                Scope::Shared(s) => format!("not a recipient of {s}"),
            });
        };
        let Some(p) = plain else {
            return Ok("plaintext missing: `gse sync` restores it".into());
        };
        if p == d.content {
            return Ok("in sync".into());
        }
        let base = self.state.base(&e.cipher);
        if base.as_deref() == Some(c.as_slice()) {
            return Ok("local changes: `gse sync` encrypts them".into());
        }
        let base_content = base
            .as_ref()
            .and_then(|b| self.decrypt(b).ok().flatten())
            .map(|b| b.content);
        Ok(if base_content.as_deref() == Some(p.as_slice()) {
            "incoming changes: `gse sync` decrypts them".into()
        } else {
            "local and incoming changes: `gse sync` merges them".into()
        })
    }

    pub fn status(&self) -> Result<()> {
        match &self.me {
            Some(me) => println!("handle: {}", me.handle),
            None => println!("handle: none (run `gse keygen`)"),
        }
        let entries = self.entries()?;
        if entries.is_empty() {
            println!("no tracked env files yet; `gse track <pattern>` adds some");
        }
        let rows: Vec<(String, String, String)> = entries
            .iter()
            .map(|e| {
                let state = self
                    .describe(e)
                    .unwrap_or_else(|err| format!("error: {err:#}"));
                (e.plain.clone(), e.scope.label(), state)
            })
            .collect();
        let w0 = rows.iter().map(|r| r.0.len()).max().unwrap_or(0);
        let w1 = rows.iter().map(|r| r.1.len()).max().unwrap_or(0);
        for (plain, scope, state) in rows {
            println!("{plain:<w0$}  {scope:<w1$}  {state}");
        }
        for scope in self.shared_scopes() {
            if let ScopeInfo::Member(list) = self.scope_info(&scope) {
                let mirror = self.abs(&format!(".gse/shared/{scope}/recipients.txt"));
                let drifted = std::fs::read_to_string(&mirror)
                    .ok()
                    .and_then(|t| RecipientList::parse(&t).ok())
                    .is_none_or(|m| m.key_set() != list.key_set());
                if drifted {
                    println!(
                        "warning: .gse/shared/{scope}/recipients.txt differs from the scope's encrypted list; `gse sync` restores it"
                    );
                }
            }
        }
        Ok(())
    }
}
