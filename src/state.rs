//! Per-worktree state in `$(git rev-parse --git-path gse)`:
//!
//! - `base/`: the last-synced ciphertext of each file, the merge base. It is
//!   ciphertext, so `.git/` holds no extra plaintext.
//! - `conflicts`: plaintext files waiting for conflict resolution.
//! - `backup/`: local edits a conflicting merge replaced.
//! - `lock`: serializes concurrent gse runs.

use crate::git::Repo;
use anyhow::{Context, Result};
use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::path::PathBuf;

pub struct State {
    pub dir: PathBuf,
    _lock: File,
}

impl State {
    pub fn open(repo: &Repo) -> Result<Self> {
        let dir = repo.git_path("gse")?;
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("cannot create {}", dir.display()))?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join("lock"))
            .context("cannot open the gse lock")?;
        lock.lock().context("cannot take the gse lock")?;
        Ok(Self { dir, _lock: lock })
    }

    fn base_path(&self, cipher: &str) -> PathBuf {
        self.dir
            .join("base")
            .join(cipher.strip_prefix(".gse/").unwrap_or(cipher))
    }

    pub fn base(&self, cipher: &str) -> Option<Vec<u8>> {
        std::fs::read(self.base_path(cipher)).ok()
    }

    pub fn set_base(&self, cipher: &str, bytes: &[u8]) -> Result<()> {
        crate::util::write_file(&self.base_path(cipher), bytes)
    }

    pub fn clear_base(&self, cipher: &str) {
        let _ = std::fs::remove_file(self.base_path(cipher));
    }

    pub fn tmp_dir(&self) -> PathBuf {
        self.dir.join("tmp")
    }

    fn conflicts_path(&self) -> PathBuf {
        self.dir.join("conflicts")
    }

    pub fn conflicts(&self) -> BTreeSet<String> {
        std::fs::read_to_string(self.conflicts_path())
            .unwrap_or_default()
            .lines()
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect()
    }

    pub fn set_conflict(&self, plain: &str, on: bool) -> Result<()> {
        let mut set = self.conflicts();
        let changed = if on {
            set.insert(plain.to_string())
        } else {
            set.remove(plain)
        };
        if changed {
            let text: String = set.iter().map(|p| format!("{p}\n")).collect();
            crate::util::write_file(&self.conflicts_path(), text.as_bytes())?;
        }
        Ok(())
    }

    /// Saves a plaintext copy before a merge replaces it and returns its path.
    pub fn backup(&self, plain: &str, bytes: &[u8]) -> Result<PathBuf> {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let path = self.dir.join("backup").join(format!("{plain}.{secs}"));
        crate::util::write_private(&path, bytes, &self.tmp_dir())?;
        Ok(path)
    }
}
