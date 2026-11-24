//! The `git` binary. gse shells out so it honors every setting git does,
//! including the temporary index (`GIT_INDEX_FILE`) that `git commit -a` and
//! `git commit <path>` hand to hooks.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub struct Repo {
    pub root: PathBuf,
}

fn run_cmd(mut cmd: Command) -> Result<Vec<u8>> {
    let out = cmd
        .stdin(Stdio::null())
        .output()
        .context("cannot run git; is it installed?")?;
    if !out.status.success() {
        bail!(
            "{:?} failed: {}",
            cmd,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(out.stdout)
}

fn split_z(bytes: &[u8]) -> Vec<String> {
    bytes
        .split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect()
}

impl Repo {
    /// The repository containing the current directory, if any.
    pub fn discover() -> Result<Option<Repo>> {
        let out = Command::new("git")
            .args(["rev-parse", "--show-toplevel"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .context("cannot run git; is it installed?")?;
        if !out.status.success() {
            return Ok(None);
        }
        let root = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if root.is_empty() {
            return Ok(None);
        }
        Ok(Some(Repo {
            root: PathBuf::from(root),
        }))
    }

    pub fn at(root: &Path) -> Repo {
        Repo {
            root: root.to_path_buf(),
        }
    }

    pub fn cmd(&self) -> Command {
        let mut cmd = Command::new("git");
        cmd.arg("-C").arg(&self.root);
        cmd
    }

    pub fn run(&self, args: &[&str]) -> Result<String> {
        let mut cmd = self.cmd();
        cmd.args(args);
        Ok(String::from_utf8_lossy(&run_cmd(cmd)?).into_owned())
    }

    pub fn succeeds(&self, args: &[&str]) -> bool {
        self.cmd()
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// A path inside the git directory, per worktree where git keeps one.
    pub fn git_path(&self, name: &str) -> Result<PathBuf> {
        let out = self.run(&["rev-parse", "--git-path", name])?;
        let path = PathBuf::from(out.trim());
        Ok(if path.is_absolute() {
            path
        } else {
            self.root.join(path)
        })
    }

    /// Stages paths, forcing past ignore rules: a `.env*` ignore pattern also
    /// matches `.gse/users/dan/.env.local.age`. Callers pass only `.gse/` paths
    /// and files gse manages.
    pub fn add(&self, paths: &[String]) -> Result<()> {
        if paths.is_empty() {
            return Ok(());
        }
        let mut cmd = self.cmd();
        cmd.args(["add", "-f", "--"]).args(paths);
        run_cmd(cmd)?;
        Ok(())
    }

    /// Removes paths from the index and the working tree.
    pub fn rm(&self, paths: &[String]) -> Result<()> {
        if paths.is_empty() {
            return Ok(());
        }
        let mut cmd = self.cmd();
        cmd.args(["rm", "-q", "-f", "--ignore-unmatch", "--"])
            .args(paths);
        run_cmd(cmd)?;
        Ok(())
    }

    pub fn is_ignored(&self, path: &str) -> bool {
        self.succeeds(&["check-ignore", "-q", "--no-index", "--", path])
    }

    /// Every path in the index: what the next commit will contain.
    pub fn indexed(&self) -> Result<Vec<String>> {
        let mut cmd = self.cmd();
        cmd.args(["ls-files", "--cached", "-z"]);
        Ok(split_z(&run_cmd(cmd)?))
    }

    /// Tracked files plus untracked files, with wholly untracked directories
    /// collapsed (`node_modules/`), which keeps hooks fast in large trees.
    pub fn candidates(&self) -> Result<Vec<String>> {
        let mut cmd = self.cmd();
        cmd.args([
            "ls-files",
            "--cached",
            "--others",
            "--directory",
            "--no-empty-directory",
            "-z",
        ]);
        Ok(split_z(&run_cmd(cmd)?))
    }

    /// Untracked files, ignored or not, for `gse init` to look through.
    pub fn untracked(&self) -> Result<Vec<String>> {
        let mut cmd = self.cmd();
        cmd.args([
            "ls-files",
            "--others",
            "--directory",
            "--no-empty-directory",
            "-z",
        ]);
        Ok(split_z(&run_cmd(cmd)?))
    }

    /// True while a rebase, merge, cherry-pick, or revert waits to finish.
    pub fn operation_in_progress(&self) -> bool {
        [
            "rebase-merge",
            "rebase-apply",
            "MERGE_HEAD",
            "CHERRY_PICK_HEAD",
            "REVERT_HEAD",
        ]
        .iter()
        .any(|name| self.git_path(name).map(|p| p.exists()).unwrap_or(false))
    }

    pub fn last_change(&self, path: &str) -> String {
        self.run(&["log", "-1", "--format=%h by %an", "--", path])
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    }
}

pub fn global_get(key: &str) -> Result<Option<String>> {
    let out = Command::new("git")
        .args(["config", "--global", "--get", key])
        .stdin(Stdio::null())
        .output()
        .context("cannot run git; is it installed?")?;
    if out.status.success() {
        Ok(Some(
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
        ))
    } else {
        Ok(None)
    }
}

pub fn global_set(key: &str, value: &str) -> Result<()> {
    let mut cmd = Command::new("git");
    cmd.args(["config", "--global", "--replace-all", key, value]);
    run_cmd(cmd)?;
    Ok(())
}

pub fn global_unset(key: &str) -> Result<()> {
    // Exit status 5 means the key was absent, which is the goal.
    let _ = Command::new("git")
        .args(["config", "--global", "--unset-all", key])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    Ok(())
}

/// `git version 2.53.0`, `2.45.1.windows.1`, and `2.39.5 (Apple Git-154)` all
/// parse to their first three numbers.
pub fn version() -> Result<(u32, u32, u32)> {
    let mut cmd = Command::new("git");
    cmd.arg("version");
    let out = String::from_utf8_lossy(&run_cmd(cmd)?).into_owned();
    parse_version(&out).with_context(|| format!("cannot parse {:?}", out.trim()))
}

pub fn parse_version(text: &str) -> Option<(u32, u32, u32)> {
    let word = text
        .split_whitespace()
        .find(|w| w.starts_with(|c: char| c.is_ascii_digit()))?;
    let mut parts = word.split('.').map(|p| p.parse::<u32>().ok());
    Some((
        parts.next()??,
        parts.next()??,
        parts.next().flatten().unwrap_or(0),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(parse_version("git version 2.53.0\n"), Some((2, 53, 0)));
        assert_eq!(
            parse_version("git version 2.45.1.windows.1"),
            Some((2, 45, 1))
        );
        assert_eq!(
            parse_version("git version 2.39.5 (Apple Git-154)"),
            Some((2, 39, 5))
        );
    }
}
