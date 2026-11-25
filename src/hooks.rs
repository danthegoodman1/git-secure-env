//! Global hook installation, hook dispatch, and `gse doctor`.
//!
//! Git 2.54 and later run hooks defined in config (`hook.<name>.command`)
//! alongside each repository's own hooks. Older versions get a global
//! `core.hooksPath` of shims that run gse and then chain to the hook that
//! would have run without it.
//!
//! Hooks and drivers run the gse binary `gse install` recorded, so they work
//! in git clients whose PATH lacks gse, such as IDEs. When that binary is
//! gone, they fall back to the gse on PATH.

use crate::config::{self, RepoConfig};
use crate::git::{self, Repo};
use crate::sync::Ctx;
use crate::ui;
use crate::util::{set_executable, write_file};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const EVENTS: [&str; 5] = [
    "pre-commit",
    "post-commit",
    "post-checkout",
    "post-merge",
    "post-rewrite",
];

/// Every hook git runs from a hooks directory. With a global hooks path, each
/// needs a shim that chains to the repository's own hook.
const ALL_HOOKS: [&str; 28] = [
    "applypatch-msg",
    "pre-applypatch",
    "post-applypatch",
    "pre-commit",
    "pre-merge-commit",
    "prepare-commit-msg",
    "commit-msg",
    "post-commit",
    "pre-rebase",
    "post-checkout",
    "post-merge",
    "pre-push",
    "pre-receive",
    "update",
    "proc-receive",
    "post-receive",
    "post-update",
    "reference-transaction",
    "push-to-checkout",
    "pre-auto-gc",
    "post-rewrite",
    "sendemail-validate",
    "fsmonitor-watchman",
    "p4-changelist",
    "p4-prepare-changelist",
    "p4-post-changelist",
    "p4-pre-submit",
    "post-index-change",
];

const CONFIG_HOOKS_SINCE: (u32, u32, u32) = (2, 54, 0);

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum HookMode {
    /// `hook.<name>.command` in global config (git 2.54 and later)
    Config,
    /// A global `core.hooksPath` of shims (any git version)
    Shims,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct InstallRecord {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mode: Option<String>,
    /// The gse binary the hooks and drivers run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    binary: Option<String>,
    /// The global `core.hooksPath` gse replaced, which the shims chain to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    previous_hooks_path: Option<String>,
}

fn record_path() -> Result<PathBuf> {
    Ok(config::config_dir()?.join("install.toml"))
}

fn load_record() -> Result<InstallRecord> {
    match std::fs::read_to_string(record_path()?) {
        Ok(text) => Ok(toml::from_str(&text).context("cannot parse install.toml")?),
        Err(_) => Ok(InstallRecord::default()),
    }
}

fn save_record(record: &InstallRecord) -> Result<()> {
    write_file(&record_path()?, toml::to_string(record)?.as_bytes())
}

fn hooks_dir() -> Result<PathBuf> {
    Ok(config::config_dir()?.join("hooks"))
}

fn slash(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// The gse binary for hooks and drivers to run: the gse on PATH when it is
/// this binary, because a package manager's link there survives upgrades,
/// else this binary.
fn this_binary() -> Result<String> {
    let exe = std::env::current_exe().context("cannot locate the gse binary")?;
    let chosen = match find_in_path("gse") {
        Some(found) if same_file(&found, &exe) => found,
        _ => exe,
    };
    Ok(slash(&chosen))
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

fn sh_quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', r"'\''"))
}

/// Shell that sets `g` to the binary `bin`, or to the gse on PATH when `bin`
/// is gone, and runs `missing` when neither exists.
fn locate(bin: &str, missing: &str) -> String {
    format!(
        "g={}; [ -x \"$g\" ] || g=$(command -v gse) || {missing}",
        sh_quote(bin)
    )
}

/// The command for a config hook. Git appends `"$@"` when the hook has
/// arguments, which this form allows, and a missing gse lets commits through.
fn hook_command(bin: &str, event: &str) -> String {
    format!("{}; \"$g\" hook {event}", locate(bin, "exit 0"))
}

fn driver_command(bin: &str) -> String {
    format!("{}; \"$g\" merge-driver %O %A %B %P", locate(bin, "g=gse"))
}

fn textconv_command(bin: &str) -> String {
    format!("{}; \"$g\" textconv", locate(bin, "g=gse"))
}

fn shim_script(name: &str, previous: Option<&str>, bin: &str) -> String {
    let mut s = String::from(
        "#!/bin/sh\n# Installed by gse (git-secure-env). `gse uninstall` removes it.\n",
    );
    if EVENTS.contains(&name) {
        s.push_str(&format!(
            "{}\nif [ -n \"$g\" ]; then\n  \"$g\" hook {name} \"$@\" || exit $?\nfi\n",
            locate(bin, "g=")
        ));
    }
    match previous {
        Some(p) => {
            let p = match p.strip_prefix("~/") {
                Some(rest) => format!("$HOME/{rest}"),
                None => p.to_string(),
            };
            s.push_str(&format!("hook=\"{p}/{name}\"\n"));
        }
        None => s.push_str(&format!(
            "hook=\"$(git rev-parse --git-common-dir 2>/dev/null)/hooks/{name}\"\n"
        )),
    }
    s.push_str("if [ -x \"$hook\" ]; then exec \"$hook\" \"$@\"; fi\nexit 0\n");
    s
}

fn install_config_hooks(bin: &str) -> Result<()> {
    for event in EVENTS {
        git::global_set(
            &format!("hook.gse-{event}.command"),
            &hook_command(bin, event),
        )?;
        git::global_set(&format!("hook.gse-{event}.event"), event)?;
    }
    Ok(())
}

fn remove_config_hooks() -> Result<()> {
    for event in EVENTS {
        git::global_unset(&format!("hook.gse-{event}.command"))?;
        git::global_unset(&format!("hook.gse-{event}.event"))?;
    }
    Ok(())
}

fn install_shims(record: &mut InstallRecord, bin: &str) -> Result<()> {
    let dir = hooks_dir()?;
    let ours = slash(&dir);
    if let Some(current) = git::global_get("core.hooksPath")?
        && current != ours
        && record.previous_hooks_path.is_none()
    {
        record.previous_hooks_path = Some(current);
    }
    std::fs::create_dir_all(&dir)?;
    for name in ALL_HOOKS {
        let path = dir.join(name);
        write_file(
            &path,
            shim_script(name, record.previous_hooks_path.as_deref(), bin).as_bytes(),
        )?;
        set_executable(&path);
    }
    git::global_set("core.hooksPath", &ours)
}

fn remove_shims(record: &mut InstallRecord) -> Result<()> {
    let dir = hooks_dir()?;
    if git::global_get("core.hooksPath")?.as_deref() == Some(slash(&dir).as_str()) {
        match record.previous_hooks_path.take() {
            Some(previous) => git::global_set("core.hooksPath", &previous)?,
            None => git::global_unset("core.hooksPath")?,
        }
    }
    if dir.exists() {
        std::fs::remove_dir_all(&dir)
            .with_context(|| format!("cannot remove {}", dir.display()))?;
    }
    Ok(())
}

fn install_drivers(bin: &str) -> Result<()> {
    git::global_set("merge.gse.name", "gse encrypted env merge")?;
    git::global_set("merge.gse.driver", &driver_command(bin))?;
    git::global_set("diff.gse.textconv", &textconv_command(bin))
}

pub fn install(mode: Option<HookMode>) -> Result<()> {
    let version = git::version()?;
    let modern = version >= CONFIG_HOOKS_SINCE;
    let mode = mode.unwrap_or(if modern {
        HookMode::Config
    } else {
        HookMode::Shims
    });
    if mode == HookMode::Config && !modern {
        bail!(
            "git {}.{}.{} predates config-based hooks (2.54); use `gse install --mode shims`",
            version.0,
            version.1,
            version.2
        );
    }
    let mut record = load_record()?;
    let bin = this_binary()?;
    install_drivers(&bin)?;
    match mode {
        HookMode::Config => {
            remove_shims(&mut record)?;
            install_config_hooks(&bin)?;
            record.mode = Some("config".into());
            ui::info("installed config-based hooks (hook.gse-*) in your global git config");
        }
        HookMode::Shims => {
            remove_config_hooks()?;
            install_shims(&mut record, &bin)?;
            record.mode = Some("shims".into());
            ui::info(format!(
                "installed hook shims in {} and set the global core.hooksPath",
                hooks_dir()?.display()
            ));
        }
    }
    record.binary = Some(bin.clone());
    save_record(&record)?;
    ui::info("installed the gse merge driver and diff textconv");
    ui::info(format!("hooks and drivers run {bin}"));
    if crate::identity::load_own()?.is_none() {
        ui::info("next: `gse keygen` creates your identity");
    }
    Ok(())
}

pub fn uninstall() -> Result<()> {
    let mut record = load_record()?;
    remove_config_hooks()?;
    remove_shims(&mut record)?;
    for key in ["merge.gse.name", "merge.gse.driver", "diff.gse.textconv"] {
        git::global_unset(key)?;
    }
    let _ = std::fs::remove_file(record_path()?);
    ui::info(
        "removed gse's hooks and drivers from your global git config; your identity stays in place",
    );
    Ok(())
}

/// `gse hook <event>`: hooks fail open except to stop a plaintext leak.
pub fn run_hook(event: &str, args: &[String]) -> i32 {
    if std::env::var_os("GSE_SKIP").is_some_and(|v| !v.is_empty()) {
        return 0;
    }
    let result = (|| -> Result<i32> {
        let Some(repo) = Repo::discover()? else {
            return Ok(0);
        };
        if !RepoConfig::path(&repo.root).exists() {
            return Ok(0);
        }
        let Some(ctx) = Ctx::open(repo)? else {
            return Ok(0);
        };
        match event {
            "pre-commit" => Ok(if ctx.pre_commit()? { 0 } else { 1 }),
            "post-commit" => {
                ctx.post_commit()?;
                Ok(0)
            }
            // A file checkout (flag 0) leaves branches alone.
            "post-checkout" if args.get(2).map(String::as_str) == Some("0") => Ok(0),
            "post-checkout" => {
                ctx.pull(true)?;
                Ok(0)
            }
            "post-merge" | "post-rewrite" => {
                ctx.pull(false)?;
                Ok(0)
            }
            _ => Ok(0),
        }
    })();
    match result {
        Ok(code) => code,
        Err(err) => {
            ui::warn(format!("{event} hook: {err:#}"));
            0
        }
    }
}

fn find_in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find_map(|dir| {
        [name.to_string(), format!("{name}.exe")]
            .into_iter()
            .map(|n| dir.join(n))
            .find(|p| p.is_file())
    })
}

struct Report {
    ok: bool,
}

impl Report {
    fn pass(&self, msg: impl std::fmt::Display) {
        println!("ok    {msg}");
    }
    fn warn(&self, msg: impl std::fmt::Display) {
        println!("warn  {msg}");
    }
    fn fail(&mut self, msg: impl std::fmt::Display) {
        println!("FAIL  {msg}");
        self.ok = false;
    }
}

pub fn doctor(self_test: bool) -> Result<bool> {
    let mut r = Report { ok: true };
    let version = git::version()?;
    let modern = version >= CONFIG_HOOKS_SINCE;
    r.pass(format!(
        "git {}.{}.{} ({})",
        version.0,
        version.1,
        version.2,
        if modern {
            "supports config-based hooks"
        } else {
            "hooks run through core.hooksPath shims"
        }
    ));

    let record = load_record()?;
    let mode = record.mode.as_deref();
    let bin = record.binary.clone().unwrap_or_default();
    match record.binary.as_deref() {
        Some(b) if Path::new(b).is_file() => {
            r.pass(format!("hooks run {b}"));
            let exe = std::env::current_exe()?;
            if !same_file(Path::new(b), &exe) {
                r.warn(format!(
                    "this gse is {}; `gse install` points the hooks at it",
                    exe.display()
                ));
            }
        }
        Some(b) => match find_in_path("gse") {
            Some(found) => r.warn(format!(
                "hooks run {b}, which is gone, so they use {} from PATH; run `gse install`",
                found.display()
            )),
            None => r.fail(format!(
                "hooks run {b}, which is gone, and gse is not on PATH; run `gse install`"
            )),
        },
        None => {}
    }
    if find_in_path("gse").is_none() {
        r.warn("gse is not on your PATH; add its directory to PATH to run it by name");
    }

    let config_hooks: Vec<Option<String>> = EVENTS
        .iter()
        .map(|e| {
            git::global_get(&format!("hook.gse-{e}.command"))
                .ok()
                .flatten()
        })
        .collect();
    let config_current = EVENTS
        .iter()
        .zip(&config_hooks)
        .all(|(e, c)| c.as_deref() == Some(hook_command(&bin, e).as_str()));
    let shims_dir = hooks_dir()?;
    let shims = git::global_get("core.hooksPath")?.as_deref() == Some(slash(&shims_dir).as_str())
        && EVENTS.iter().all(|e| shims_dir.join(e).is_file());
    let shims_current = EVENTS.iter().all(|e| {
        std::fs::read_to_string(shims_dir.join(e)).ok()
            == Some(shim_script(e, record.previous_hooks_path.as_deref(), &bin))
    });
    let stale = "hooks predate this gse; run `gse install` to update them";
    match (mode, config_hooks.iter().all(Option::is_some), shims) {
        (Some("config"), true, _) if modern => {
            r.pass("hooks installed (config-based)");
            if !config_current {
                r.warn(stale);
            }
        }
        (Some("config"), true, _) => r.fail(
            "config-based hooks are installed, but this git predates 2.54; run `gse install`",
        ),
        (Some("shims"), _, true) => {
            r.pass("hooks installed (core.hooksPath shims)");
            if !shims_current {
                r.warn(stale);
            }
            if modern {
                r.warn("git 2.54+ supports config-based hooks; `gse install` migrates to them");
            }
        }
        _ => r.fail("hooks are not installed; run `gse install`"),
    }

    let driver = git::global_get("merge.gse.driver")?;
    let textconv = git::global_get("diff.gse.textconv")?;
    match (&driver, &textconv) {
        (Some(d), Some(t)) => {
            r.pass("merge driver and diff textconv configured");
            if *d != driver_command(&bin) || *t != textconv_command(&bin) {
                r.warn("the merge driver and diff textconv predate this gse; run `gse install`");
            }
        }
        _ => r.fail("merge driver or diff textconv missing; run `gse install`"),
    }

    let me = crate::sync::load_me()?;
    match (&me, crate::identity::load_own()?) {
        (Some(me), Some(_)) => r.pass(format!(
            "identity {} (handle {}, {} personal recipient key{})",
            config::identity_path()?.display(),
            me.handle,
            me.personal.key_set().len(),
            if me.personal.key_set().len() == 1 { "" } else { "s" }
        )),
        _ => r.fail("no gse identity; run `gse keygen` (or `gse keygen --import` with your key from another machine)"),
    }

    if let Some(repo) = Repo::discover()?
        && let Some(cfg) = RepoConfig::load(&repo.root)?
    {
        let local = repo
            .run(&["config", "--local", "--get", "core.hooksPath"])
            .ok();
        let local = local
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        match (&local, mode) {
            (Some(path), Some("shims")) => r.fail(format!(
                "this repository sets core.hooksPath={path} (husky does this), which overrides gse's global shims. \
                 Add `gse hook <event> \"$@\"` to its pre-commit, post-checkout, post-merge, and post-rewrite hooks, \
                 or upgrade to git 2.54 and run `gse install`."
            )),
            _ => r.pass("repository hooks path leaves gse's hooks in effect"),
        }
        for rule in &cfg.files {
            let sample = rule.path.strip_prefix("**/").unwrap_or(&rule.path);
            if !sample.contains(['*', '?', '[', '{']) && !repo.is_ignored(sample) {
                r.fail(format!(
                    "{sample} is not gitignored; `gse track {}` adds the rule",
                    rule.path
                ));
            }
        }
        if !repo.root.join(".gse/.gitattributes").exists() {
            r.fail(".gse/.gitattributes is missing; `gse init` restores it");
        }
    }

    if self_test {
        if r.ok {
            match run_self_test() {
                Ok(()) => r.pass("self-test: a commit in one clone decrypted in another"),
                Err(err) => r.fail(format!("self-test: {err:#}")),
            }
        } else {
            r.warn("self-test skipped until the failures above are fixed");
        }
    }
    Ok(r.ok)
}

fn git_in(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .current_dir(dir)
        .args([
            "-c",
            "user.name=gse self-test",
            "-c",
            "user.email=gse@localhost",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "tag.gpgsign=false",
        ])
        .args(args)
        .stdin(Stdio::null())
        .env("GSE_NO_SSH", "1")
        .output()
        .context("cannot run git")?;
    if !out.status.success() {
        bail!(
            "git {} failed: {}{}",
            args.join(" "),
            String::from_utf8_lossy(&out.stdout).trim(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Commits an env file in a throwaway repository through the real hooks,
/// clones it, and checks that the clone decrypts the same content.
fn run_self_test() -> Result<()> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("gse-self-test-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let result = (|| -> Result<()> {
        let exe = std::env::current_exe()?;
        git_in(
            &dir,
            &[
                "init",
                "-q",
                "--bare",
                "--initial-branch=main",
                "remote.git",
            ],
        )?;
        git_in(&dir, &["clone", "-q", "remote.git", "a"])?;
        let a = dir.join("a");
        git_in(&a, &["symbolic-ref", "HEAD", "refs/heads/main"])?;
        let secret = format!("GSE_SELF_TEST={nanos}\n");
        std::fs::write(a.join(".env.local"), &secret)?;
        let out = Command::new(&exe)
            .current_dir(&a)
            .args(["init", "--no-agents"])
            .env("GSE_NO_SSH", "1")
            .output()?;
        if !out.status.success() {
            bail!(
                "gse init failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        git_in(&a, &["commit", "-q", "-m", "gse self-test"])?;
        let committed = git_in(&a, &["ls-files", ".gse/users"])?;
        if !committed.lines().any(|l| l.ends_with(".env.local.age")) {
            bail!("the commit holds no encrypted .env.local; is the pre-commit hook running?");
        }
        git_in(&a, &["push", "-q", "origin", "main"])?;
        git_in(&dir, &["clone", "-q", "remote.git", "b"])?;
        let got = std::fs::read_to_string(dir.join("b/.env.local"))
            .context("the clone has no .env.local; is the post-checkout hook running?")?;
        if got != secret {
            bail!("the clone decrypted different content");
        }
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&dir);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shims_chain_to_the_repository_hook() {
        let s = shim_script("pre-commit", None, "/opt/gse");
        assert!(s.contains("\"$g\" hook pre-commit \"$@\" || exit $?"));
        assert!(s.contains("--git-common-dir"));
        let s = shim_script("pre-push", Some("~/hooks"), "/opt/gse");
        assert!(!s.contains("hook pre-push"));
        assert!(s.contains("hook=\"$HOME/hooks/pre-push\""));
    }

    fn sh(script: &str) -> String {
        let out = Command::new("sh").args(["-c", script]).output().unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    #[cfg(unix)]
    #[test]
    fn hooks_prefer_the_recorded_binary_and_fall_back_to_path() {
        let found = sh(&format!("{}; echo \"$g\"", locate("/bin/sh", "exit 0")));
        assert_eq!(found, "/bin/sh\n");
        let fallback = sh(&format!(
            "PATH=/nonexistent; {}; echo \"found $g\"",
            locate("/nonexistent/gse", "exit 0")
        ));
        assert_eq!(fallback, "");
        assert_eq!(sh_quote("/it's/gse"), r"'/it'\''s/gse'");
    }
}
