//! Recipient lists: public keys grouped by owner.
//!
//! On disk a list is a valid age recipients file. An owner line,
//! `# @github:alice (refreshed 2026-11-24)`, starts a block, and the keys
//! below it belong to that owner until the next owner line.

use anyhow::{Result, anyhow, bail};
use std::collections::BTreeSet;
use std::str::FromStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OwnerKind {
    Github,
    Key,
}

impl OwnerKind {
    pub fn as_str(self) -> &'static str {
        match self {
            OwnerKind::Github => "github",
            OwnerKind::Key => "key",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "github" => Some(OwnerKind::Github),
            "key" => Some(OwnerKind::Key),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Owner {
    pub kind: OwnerKind,
    pub name: String,
    pub note: Option<String>,
    pub keys: Vec<String>,
    pub unsupported: Vec<String>,
}

impl Owner {
    pub fn new(kind: OwnerKind, name: &str) -> Self {
        Self {
            kind,
            name: name.to_string(),
            note: None,
            keys: Vec::new(),
            unsupported: Vec::new(),
        }
    }

    pub fn label(&self) -> String {
        format!("@{}:{}", self.kind.as_str(), self.name)
    }

    fn parse_label(label: &str) -> Option<(OwnerKind, String)> {
        let (kind, name) = label.strip_prefix('@')?.split_once(':')?;
        let kind = OwnerKind::parse(kind)?;
        crate::config::valid_owner_name(name).then(|| (kind, name.to_string()))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecipientList {
    pub owners: Vec<Owner>,
}

pub enum KeyCheck {
    Supported,
    Unsupported(String),
    Invalid(String),
}

/// `ssh-ed25519 AAAA… comment` becomes `ssh-ed25519 AAAA…`; age keys stay whole.
pub fn normalize_key(line: &str) -> String {
    let mut fields = line.split_whitespace();
    match (fields.next(), fields.next()) {
        (Some(a), _) if a.starts_with("age1") => a.to_string(),
        (Some(a), Some(b)) => format!("{a} {b}"),
        (Some(a), None) => a.to_string(),
        _ => String::new(),
    }
}

pub fn short(key: &str) -> String {
    if key.chars().count() > 32 {
        format!("{}…", key.chars().take(32).collect::<String>())
    } else {
        key.to_string()
    }
}

pub fn check_key(key: &str) -> KeyCheck {
    use age::ssh::ParseRecipientKeyError as E;
    if key.starts_with("age1") {
        return match age::x25519::Recipient::from_str(key) {
            Ok(_) => KeyCheck::Supported,
            Err(e) => KeyCheck::Invalid(e.to_string()),
        };
    }
    match age::ssh::Recipient::from_str(key) {
        Ok(_) => KeyCheck::Supported,
        Err(E::Unsupported(kind)) => KeyCheck::Unsupported(kind),
        Err(E::Ignore) => KeyCheck::Unsupported(key.split(' ').next().unwrap_or("").into()),
        Err(E::RsaModulusTooSmall) => KeyCheck::Unsupported("ssh-rsa under 2048 bits".into()),
        Err(E::RsaModulusTooLarge) => KeyCheck::Unsupported("ssh-rsa over 16384 bits".into()),
        Err(E::Invalid(msg)) => KeyCheck::Invalid(msg.to_string()),
        Err(_) => KeyCheck::Invalid("unrecognized key".into()),
    }
}

pub fn to_recipient(key: &str) -> Result<Box<dyn age::Recipient>> {
    if key.starts_with("age1") {
        let r = age::x25519::Recipient::from_str(key)
            .map_err(|e| anyhow!("invalid age key {}: {e}", short(key)))?;
        Ok(Box::new(r))
    } else {
        let r = age::ssh::Recipient::from_str(key)
            .map_err(|e| anyhow!("unsupported key {}: {e:?}", short(key)))?;
        Ok(Box::new(r))
    }
}

impl RecipientList {
    pub fn parse(text: &str) -> Result<Self> {
        let mut list = Self::default();
        for (i, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() {
                continue;
            }
            if let Some(comment) = line.strip_prefix('#') {
                let comment = comment.trim();
                if comment.starts_with('@') {
                    let (label, note) = match comment.split_once(" (") {
                        Some((label, rest)) => {
                            (label.trim(), Some(rest.trim_end_matches(')').to_string()))
                        }
                        None => (comment, None),
                    };
                    let (kind, name) = Owner::parse_label(label)
                        .ok_or_else(|| anyhow!("line {}: invalid owner {label:?}", i + 1))?;
                    let mut owner = Owner::new(kind, &name);
                    owner.note = note;
                    list.owners.push(owner);
                } else if let Some(key) = comment.strip_prefix("unsupported by age:")
                    && let Some(owner) = list.owners.last_mut()
                {
                    owner.unsupported.push(normalize_key(key));
                }
                continue;
            }
            let key = normalize_key(line);
            match check_key(&key) {
                KeyCheck::Supported => {}
                KeyCheck::Unsupported(why) | KeyCheck::Invalid(why) => {
                    bail!("line {}: {} ({why})", i + 1, short(&key))
                }
            }
            if list.owners.is_empty() {
                list.owners.push(Owner::new(OwnerKind::Key, "unnamed"));
            }
            let owner = list.owners.last_mut().expect("an owner exists");
            if !owner.keys.contains(&key) {
                owner.keys.push(key);
            }
        }
        Ok(list)
    }

    /// Renders the list as an age recipients file under the given comment lines.
    pub fn render(&self, header: &[&str]) -> String {
        let mut out = String::new();
        for line in header {
            out.push_str("# ");
            out.push_str(line);
            out.push('\n');
        }
        for owner in &self.owners {
            out.push('\n');
            out.push_str("# ");
            out.push_str(&owner.label());
            if let Some(note) = &owner.note {
                out.push_str(&format!(" ({note})"));
            }
            out.push('\n');
            for key in &owner.keys {
                out.push_str(key);
                out.push('\n');
            }
            for key in &owner.unsupported {
                out.push_str(&format!("# unsupported by age: {key}\n"));
            }
        }
        out
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.owners
            .iter()
            .flat_map(|o| o.keys.iter().map(String::as_str))
    }

    pub fn key_set(&self) -> BTreeSet<String> {
        self.keys().map(str::to_string).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.keys().next().is_none()
    }

    fn pairs(&self) -> BTreeSet<(String, String)> {
        self.owners
            .iter()
            .flat_map(|o| o.keys.iter().map(move |k| (o.label(), k.clone())))
            .collect()
    }

    /// Replaces the owner with the same kind and name, or appends it.
    pub fn upsert(&mut self, owner: Owner) {
        match self
            .owners
            .iter_mut()
            .find(|o| o.kind == owner.kind && o.name == owner.name)
        {
            Some(existing) => *existing = owner,
            None => self.owners.push(owner),
        }
    }

    /// Removes every owner with this name (`alice`, `@github:alice`, or
    /// `github:alice`) and returns how many it removed.
    pub fn remove(&mut self, name: &str) -> usize {
        let name = name.trim_start_matches('@');
        let before = self.owners.len();
        self.owners
            .retain(|o| o.name != name && format!("{}:{}", o.kind.as_str(), o.name) != name);
        before - self.owners.len()
    }

    pub fn recipients(&self) -> Result<Vec<Box<dyn age::Recipient>>> {
        self.keys().map(to_recipient).collect()
    }

    /// Lines describing keys added (`+`) and removed (`-`) going from `self`
    /// to `newer`.
    pub fn changes(&self, newer: &Self) -> Vec<String> {
        let old = self.pairs();
        let new = newer.pairs();
        let mut out = Vec::new();
        for (label, key) in new.difference(&old) {
            out.push(format!("+ {} {label}", short(key)));
        }
        for (label, key) in old.difference(&new) {
            out.push(format!("- {} {label}", short(key)));
        }
        out
    }

    /// Three-way merge: both sides' additions and removals applied to `base`.
    pub fn merge3(base: &Self, ours: &Self, theirs: &Self) -> Self {
        let b = base.pairs();
        let o = ours.pairs();
        let t = theirs.pairs();
        let mut result: BTreeSet<(String, String)> = b.clone();
        result.extend(o.difference(&b).cloned());
        result.extend(t.difference(&b).cloned());
        for gone in b.difference(&o).chain(b.difference(&t)) {
            result.remove(gone);
        }
        let mut merged = Self::default();
        for owner in ours.owners.iter().chain(&theirs.owners).chain(&base.owners) {
            let label = owner.label();
            if merged.owners.iter().any(|o| o.label() == label) {
                continue;
            }
            let mut keys: Vec<String> = Vec::new();
            for list in [ours, theirs, base] {
                for o in list.owners.iter().filter(|o| o.label() == label) {
                    for k in &o.keys {
                        if result.contains(&(label.clone(), k.clone())) && !keys.contains(k) {
                            keys.push(k.clone());
                        }
                    }
                }
            }
            if !keys.is_empty() {
                let mut kept = owner.clone();
                kept.keys = keys;
                merged.owners.push(kept);
            }
        }
        merged
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> String {
        age::x25519::Identity::generate().to_public().to_string()
    }

    #[test]
    fn parse_and_render_round_trip() {
        let (a, b) = (key(), key());
        let text = format!(
            "# title\n\n# @github:alice (refreshed 2026-11-24)\n{a}\n# unsupported by age: ecdsa-sha2-nistp256 AAAA\n\n# @key:ci\n{b}\n"
        );
        let list = RecipientList::parse(&text).unwrap();
        assert_eq!(list.owners.len(), 2);
        assert_eq!(list.owners[0].note.as_deref(), Some("refreshed 2026-11-24"));
        assert_eq!(list.owners[0].unsupported.len(), 1);
        let again = RecipientList::parse(&list.render(&["title"])).unwrap();
        assert_eq!(again, list);
    }

    #[test]
    fn rejects_garbage_keys() {
        assert!(RecipientList::parse("not-a-key\n").is_err());
    }

    #[test]
    fn merge3_applies_both_sides() {
        let (k1, k2, k3, k4) = (key(), key(), key(), key());
        let mk = |pairs: &[(&str, &str)]| {
            let mut l = RecipientList::default();
            for (name, k) in pairs {
                let mut o = l
                    .owners
                    .iter()
                    .find(|o| o.name == *name)
                    .cloned()
                    .unwrap_or_else(|| Owner::new(OwnerKind::Key, name));
                o.keys.push(k.to_string());
                l.upsert(o);
            }
            l
        };
        let base = mk(&[("a", &k1), ("b", &k2)]);
        let ours = mk(&[("a", &k1), ("b", &k2), ("c", &k3)]);
        let theirs = mk(&[("a", &k1), ("d", &k4)]);
        let merged = RecipientList::merge3(&base, &ours, &theirs);
        let keys = merged.key_set();
        assert!(keys.contains(&k1) && keys.contains(&k3) && keys.contains(&k4));
        assert!(!keys.contains(&k2));
    }

    #[test]
    fn remove_by_name_or_label() {
        let mut l = RecipientList::default();
        let mut o = Owner::new(OwnerKind::Github, "alice");
        o.keys.push(key());
        l.upsert(o);
        assert_eq!(l.remove("@github:alice"), 1);
        assert!(l.is_empty());
    }
}
