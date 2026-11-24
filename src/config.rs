//! Machine configuration in `~/.config/gse/` and repository configuration in
//! `.gse/config.toml`.

use anyhow::{Context, Result, bail};
use globset::{GlobBuilder, GlobMatcher};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The reserved scope name that gives each developer a directory of their own.
pub const USER_SCOPE: &str = "user";

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

pub fn home_dir() -> Option<PathBuf> {
    env_path("HOME").or_else(|| env_path("USERPROFILE"))
}

/// `GSE_CONFIG_DIR`, else `$XDG_CONFIG_HOME/gse`, else `~/.config/gse`, on
/// every platform.
pub fn config_dir() -> Result<PathBuf> {
    if let Some(dir) = env_path("GSE_CONFIG_DIR") {
        return Ok(dir);
    }
    if let Some(dir) = env_path("XDG_CONFIG_HOME") {
        return Ok(dir.join("gse"));
    }
    let home = home_dir().context("cannot find your home directory; set HOME or GSE_CONFIG_DIR")?;
    Ok(home.join(".config").join("gse"))
}

pub fn identity_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("identity.txt"))
}

pub fn personal_recipients_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("recipients.txt"))
}

/// Handles and shared scope names: lowercase letters, digits, `.`, `_`, `-`.
pub fn valid_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && !s.starts_with('.')
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
}

/// Owner names in recipient lists also allow uppercase, as GitHub usernames do.
pub fn valid_owner_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalConfig {
    pub handle: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github: Option<String>,
}

impl GlobalConfig {
    pub fn path() -> Result<PathBuf> {
        Ok(config_dir()?.join("config.toml"))
    }

    pub fn load() -> Result<Option<Self>> {
        let path = Self::path()?;
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Ok(None);
        };
        let cfg: Self =
            toml::from_str(&text).with_context(|| format!("cannot parse {}", path.display()))?;
        Ok(Some(cfg))
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path()?;
        let body = toml::to_string(self)?;
        crate::util::write_file(
            &path,
            format!("# gse machine configuration. `gse keygen` writes it.\n{body}").as_bytes(),
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileRule {
    pub path: String,
    pub scope: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoConfig {
    pub version: u32,
    #[serde(default, rename = "file", skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<FileRule>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub stacks: BTreeMap<String, Vec<String>>,
}

impl Default for RepoConfig {
    fn default() -> Self {
        Self {
            version: 1,
            files: Vec::new(),
            stacks: BTreeMap::new(),
        }
    }
}

impl RepoConfig {
    pub fn path(root: &Path) -> PathBuf {
        root.join(".gse").join("config.toml")
    }

    pub fn load(root: &Path) -> Result<Option<Self>> {
        let path = Self::path(root);
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Ok(None);
        };
        let cfg: Self =
            toml::from_str(&text).with_context(|| format!("cannot parse {}", path.display()))?;
        if cfg.version != 1 {
            bail!(
                "{} has version {}; this gse understands version 1, so upgrade gse",
                path.display(),
                cfg.version
            );
        }
        for rule in &cfg.files {
            if rule.scope != USER_SCOPE && !valid_name(&rule.scope) {
                bail!("invalid scope name {:?} in {}", rule.scope, path.display());
            }
        }
        Ok(Some(cfg))
    }

    pub fn save(&self, root: &Path) -> Result<()> {
        let body = toml::to_string(self)?;
        let text = format!(
            "# Tracked env files and the scope each belongs to. `gse track` and\n\
             # `gse untrack` edit this file; see .gse/README.md.\n{body}"
        );
        crate::util::write_file(&Self::path(root), text.as_bytes())
    }

    pub fn matcher(&self) -> Result<Matcher> {
        let mut rules = Vec::new();
        for rule in &self.files {
            rules.push((compile_glob(&rule.path)?, rule.scope.clone()));
        }
        Ok(Matcher { rules })
    }

    /// The stack's files, or `.env` then `.env.local` for an undeclared
    /// `default` stack.
    pub fn stack(&self, name: &str) -> Result<Vec<String>> {
        if let Some(files) = self.stacks.get(name) {
            return Ok(files.clone());
        }
        if name == "default" {
            return Ok(vec![".env".into(), ".env.local".into()]);
        }
        bail!("no stack named {name:?} in .gse/config.toml");
    }
}

pub fn compile_glob(pattern: &str) -> Result<GlobMatcher> {
    Ok(GlobBuilder::new(pattern)
        .literal_separator(true)
        .build()
        .with_context(|| format!("invalid pattern {pattern:?}"))?
        .compile_matcher())
}

pub struct Matcher {
    rules: Vec<(GlobMatcher, String)>,
}

impl Matcher {
    /// The scope of the first rule matching a repository-relative path.
    pub fn scope_for(&self, path: &str) -> Option<&str> {
        self.rules
            .iter()
            .find(|(glob, _)| glob.is_match(path))
            .map(|(_, scope)| scope.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs_match_at_any_depth_only_with_double_star() {
        let cfg = RepoConfig {
            files: vec![
                FileRule {
                    path: "**/.env.local".into(),
                    scope: "user".into(),
                },
                FileRule {
                    path: ".env".into(),
                    scope: "team".into(),
                },
            ],
            ..Default::default()
        };
        let m = cfg.matcher().unwrap();
        assert_eq!(m.scope_for(".env.local"), Some("user"));
        assert_eq!(m.scope_for("apps/web/.env.local"), Some("user"));
        assert_eq!(m.scope_for(".env"), Some("team"));
        assert_eq!(m.scope_for("apps/.env"), None);
    }

    #[test]
    fn names() {
        assert!(valid_name("dan"));
        assert!(valid_name("ci-2"));
        assert!(!valid_name("Dan"));
        assert!(!valid_name(".hidden"));
        assert!(valid_owner_name("danthegoodman1"));
        assert!(valid_owner_name("Dan_G"));
        assert!(!valid_owner_name("a b"));
    }

    #[test]
    fn config_round_trips() {
        let mut cfg = RepoConfig::default();
        cfg.files.push(FileRule {
            path: ".env.local".into(),
            scope: "user".into(),
        });
        cfg.stacks
            .insert("ci".into(), vec![".env".into(), ".env.ci".into()]);
        let text = toml::to_string(&cfg).unwrap();
        let back: RepoConfig = toml::from_str(&text).unwrap();
        assert_eq!(back.files, cfg.files);
        assert_eq!(back.stacks, cfg.stacks);
    }
}
