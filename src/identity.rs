//! Private keys on this machine.
//!
//! gse tries, in order: `GSE_IDENTITY` (key text, for CI), `GSE_IDENTITY_FILE`,
//! `~/.config/gse/identity.txt`, and each `~/.ssh` key pair whose public half
//! matches a file's recipients. SSH keys load lazily, so gse asks for a
//! passphrase only for a file that key can open.

use crate::config;
use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use sha2::{Digest, Sha256};
use std::cell::OnceCell;
use std::path::{Path, PathBuf};
use std::str::FromStr;

pub struct SshKey {
    pub path: PathBuf,
    pub kind: String,
    pub tag: [u8; 4],
    loaded: OnceCell<Option<age::ssh::Identity>>,
}

impl SshKey {
    pub fn get(&self) -> Option<&age::ssh::Identity> {
        self.loaded.get_or_init(|| load_ssh(&self.path)).as_ref()
    }
}

#[derive(Default)]
pub struct Identities {
    pub native: Vec<age::x25519::Identity>,
    pub ssh_inline: Vec<age::ssh::Identity>,
    pub ssh: Vec<SshKey>,
    /// Where each identity came from, for `gse doctor`.
    pub sources: Vec<String>,
}

impl Identities {
    pub fn load() -> Result<Self> {
        let mut ids = Self::default();
        if let Ok(text) = std::env::var("GSE_IDENTITY")
            && !text.trim().is_empty()
        {
            ids.add_text(&text, "GSE_IDENTITY")
                .context("cannot parse GSE_IDENTITY")?;
        }
        if let Some(path) = std::env::var_os("GSE_IDENTITY_FILE").filter(|v| !v.is_empty()) {
            let path = PathBuf::from(path);
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("cannot read GSE_IDENTITY_FILE {}", path.display()))?;
            ids.add_text(&text, &path.display().to_string())?;
        }
        let own = config::identity_path()?;
        if let Ok(text) = std::fs::read_to_string(&own) {
            ids.add_text(&text, &own.display().to_string())
                .with_context(|| format!("cannot parse {}", own.display()))?;
        }
        if std::env::var_os("GSE_NO_SSH").is_none_or(|v| v.is_empty())
            && let Some(home) = config::home_dir()
        {
            ids.ssh = discover_ssh(&home.join(".ssh"));
            for key in &ids.ssh {
                ids.sources.push(key.path.display().to_string());
            }
        }
        Ok(ids)
    }

    fn add_text(&mut self, text: &str, source: &str) -> Result<()> {
        if text.contains("-----BEGIN") {
            let id = age::ssh::Identity::from_buffer(text.as_bytes(), Some(source.to_string()))?;
            match id {
                age::ssh::Identity::Unencrypted(_) => self.ssh_inline.push(id),
                _ => bail!("{source} holds an encrypted or unsupported SSH key"),
            }
            self.sources.push(source.to_string());
            return Ok(());
        }
        let mut found = false;
        for line in text.lines().map(str::trim) {
            if line.starts_with("AGE-SECRET-KEY-") {
                let id = age::x25519::Identity::from_str(line)
                    .map_err(|e| anyhow!("invalid age secret key in {source}: {e}"))?;
                self.native.push(id);
                found = true;
            }
        }
        if !found {
            bail!("{source} holds no AGE-SECRET-KEY line");
        }
        self.sources.push(source.to_string());
        Ok(())
    }
}

/// Reads the gse identity file and returns its key.
pub fn load_own() -> Result<Option<age::x25519::Identity>> {
    let path = config::identity_path()?;
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(None);
    };
    parse_secret(&text)
        .map(Some)
        .with_context(|| format!("cannot parse {}", path.display()))
}

pub fn parse_secret(text: &str) -> Result<age::x25519::Identity> {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("AGE-SECRET-KEY-"))
        .ok_or_else(|| anyhow!("no AGE-SECRET-KEY line"))?;
    age::x25519::Identity::from_str(line).map_err(|e| anyhow!("invalid age secret key: {e}"))
}

/// The 4-byte tag age puts in `ssh-ed25519` and `ssh-rsa` stanzas.
pub fn ssh_tag(wire: &[u8]) -> [u8; 4] {
    let digest = Sha256::digest(wire);
    [digest[0], digest[1], digest[2], digest[3]]
}

fn discover_ssh(dir: &Path) -> Vec<SshKey> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut keys = Vec::new();
    let mut names: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    names.sort();
    for pub_path in names {
        let Some(name) = pub_path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(stem) = name.strip_suffix(".pub") else {
            continue;
        };
        let private = dir.join(stem);
        if !private.is_file() {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&pub_path) else {
            continue;
        };
        let mut fields = text.split_whitespace();
        let (Some(kind), Some(b64)) = (fields.next(), fields.next()) else {
            continue;
        };
        if kind != "ssh-ed25519" && kind != "ssh-rsa" {
            continue;
        }
        let Ok(wire) = STANDARD.decode(b64) else {
            continue;
        };
        keys.push(SshKey {
            path: private,
            kind: kind.to_string(),
            tag: ssh_tag(&wire),
            loaded: OnceCell::new(),
        });
    }
    keys
}

fn load_ssh(path: &Path) -> Option<age::ssh::Identity> {
    let file = std::fs::File::open(path).ok()?;
    let reader = std::io::BufReader::new(file);
    let id = age::ssh::Identity::from_buffer(reader, Some(path.display().to_string())).ok()?;
    match id {
        age::ssh::Identity::Unencrypted(_) => Some(id),
        age::ssh::Identity::Encrypted(enc) => {
            let prompt = format!("gse: passphrase for {}: ", path.display());
            let passphrase = match rpassword::prompt_password(prompt) {
                Ok(p) => p,
                Err(_) => {
                    crate::ui::warn(format!(
                        "skipping {}: it needs a passphrase and there is no terminal",
                        path.display()
                    ));
                    return None;
                }
            };
            match enc.decrypt(passphrase.into()) {
                Ok(key) => Some(age::ssh::Identity::Unencrypted(key)),
                Err(_) => {
                    crate::ui::warn(format!("wrong passphrase for {}", path.display()));
                    None
                }
            }
        }
        age::ssh::Identity::Unsupported(_) => None,
    }
}
