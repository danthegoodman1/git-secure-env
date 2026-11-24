//! Public SSH keys from `https://github.com/<user>.keys`.
//!
//! `GSE_GITHUB_URL` replaces the base URL; a `file://<dir>` value reads
//! `<dir>/<user>.keys` instead, for tests and offline mirrors.

use crate::recipients::{KeyCheck, Owner, OwnerKind, check_key, normalize_key};
use anyhow::{Context, Result, bail};

pub fn fetch_keys(user: &str) -> Result<Vec<String>> {
    if !crate::config::valid_owner_name(user) {
        bail!("{user:?} is not a valid GitHub username");
    }
    let base = std::env::var("GSE_GITHUB_URL").unwrap_or_else(|_| "https://github.com".into());
    let body = if let Some(dir) = base.strip_prefix("file://") {
        let path = std::path::Path::new(dir).join(format!("{user}.keys"));
        std::fs::read_to_string(&path).with_context(|| format!("cannot read {}", path.display()))?
    } else {
        let url = format!("{}/{user}.keys", base.trim_end_matches('/'));
        match ureq::get(&url).header("User-Agent", "gse").call() {
            Ok(mut resp) => resp
                .body_mut()
                .read_to_string()
                .with_context(|| format!("cannot read {url}"))?,
            Err(ureq::Error::StatusCode(404)) => bail!("GitHub user {user} not found"),
            Err(e) => return Err(e).with_context(|| format!("cannot fetch {url}")),
        }
    };
    Ok(body
        .lines()
        .map(normalize_key)
        .filter(|k| !k.is_empty())
        .collect())
}

/// An owner block for a GitHub user: keys age supports, plus the rest
/// recorded as unsupported.
pub fn owner(user: &str) -> Result<Owner> {
    let mut owner = Owner::new(OwnerKind::Github, user);
    owner.note = Some(format!("refreshed {}", crate::util::today()));
    for key in fetch_keys(user)? {
        match check_key(&key) {
            KeyCheck::Supported => owner.keys.push(key),
            KeyCheck::Unsupported(_) | KeyCheck::Invalid(_) => owner.unsupported.push(key),
        }
    }
    Ok(owner)
}
