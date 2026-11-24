//! End-to-end tests: real `git`, real hooks, one bare remote, and "machines"
//! that each have their own home directory and global git config. They run on
//! Unix; Windows CI builds gse and runs its unit tests.
#![cfg(unix)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

struct World {
    dir: TempDir,
}

struct Machine {
    name: String,
    home: PathBuf,
    root: PathBuf,
    github: PathBuf,
}

fn gse_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_gse"))
}

fn path_env() -> OsString {
    let mut paths = vec![gse_bin().parent().unwrap().to_path_buf()];
    if let Some(p) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&p));
    }
    std::env::join_paths(paths).unwrap()
}

fn text(out: &Output) -> String {
    format!(
        "status: {}\nstdout:\n{}\nstderr:\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

impl World {
    fn new() -> World {
        let w = World {
            dir: tempfile::tempdir().unwrap(),
        };
        std::fs::create_dir_all(w.dir.path().join("github")).unwrap();
        let out = Command::new("git")
            .args(["init", "-q", "--bare", "--initial-branch=main"])
            .arg(w.remote())
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", text(&out));
        w
    }

    fn remote(&self) -> PathBuf {
        self.dir.path().join("remote.git")
    }

    /// A machine whose user is `name`, with gse installed in `mode`.
    fn machine(&self, id: &str, name: &str) -> Machine {
        let root = self.dir.path().join(id);
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let m = Machine {
            name: name.to_string(),
            home,
            root,
            github: self.dir.path().join("github"),
        };
        m.git(
            &m.root,
            &["config", "--global", "init.defaultBranch", "main"],
        );
        m
    }
}

impl Machine {
    fn env(&self, cmd: &mut Command) {
        cmd.env_clear()
            .env("PATH", path_env())
            .env("HOME", &self.home)
            .env("USER", &self.name)
            .env("LANG", "C")
            .env("GIT_CONFIG_GLOBAL", self.home.join(".gitconfig"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", &self.name)
            .env("GIT_AUTHOR_EMAIL", format!("{}@example.com", self.name))
            .env("GIT_COMMITTER_NAME", &self.name)
            .env("GIT_COMMITTER_EMAIL", format!("{}@example.com", self.name))
            .env(
                "GSE_GITHUB_URL",
                format!("file://{}", self.github.display()),
            );
        if let Some(tmp) = std::env::var_os("TMPDIR") {
            cmd.env("TMPDIR", tmp);
        }
    }

    fn run_raw(&self, program: &Path, cwd: &Path, args: &[&str], extra: &[(&str, &str)]) -> Output {
        let mut cmd = Command::new(program);
        self.env(&mut cmd);
        for (k, v) in extra {
            cmd.env(k, v);
        }
        cmd.current_dir(cwd).args(args).output().unwrap()
    }

    fn git_out(&self, cwd: &Path, args: &[&str]) -> Output {
        self.run_raw(Path::new("git"), cwd, args, &[])
    }

    fn git(&self, cwd: &Path, args: &[&str]) -> String {
        let out = self.git_out(cwd, args);
        assert!(out.status.success(), "git {args:?}\n{}", text(&out));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn gse_out(&self, cwd: &Path, args: &[&str]) -> Output {
        self.run_raw(&gse_bin(), cwd, args, &[])
    }

    fn gse(&self, cwd: &Path, args: &[&str]) -> String {
        let out = self.gse_out(cwd, args);
        assert!(out.status.success(), "gse {args:?}\n{}", text(&out));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn setup(&self) -> &Self {
        self.gse(&self.root, &["install"]);
        self.gse(&self.root, &["keygen", "--handle", &self.name]);
        self
    }

    /// Installs gse and imports another machine's identity.
    fn setup_as(&self, other: &Machine) -> &Self {
        self.gse(&self.root, &["install"]);
        let id = other.home.join(".config/gse/identity.txt");
        self.gse(
            &self.root,
            &[
                "keygen",
                "--handle",
                &other.name,
                "--import",
                id.to_str().unwrap(),
            ],
        );
        self
    }

    fn clone(&self, world: &World, dir: &str) -> PathBuf {
        let out = self.git_out(
            &self.root,
            &["clone", "-q", world.remote().to_str().unwrap(), dir],
        );
        assert!(out.status.success(), "{}", text(&out));
        let repo = self.root.join(dir);
        if self.git_out(&repo, &["rev-parse", "HEAD"]).status.success() {
            return repo;
        }
        self.git(&repo, &["symbolic-ref", "HEAD", "refs/heads/main"]);
        repo
    }

    fn public_key(&self) -> String {
        let text = std::fs::read_to_string(self.home.join(".config/gse/identity.txt")).unwrap();
        text.lines()
            .find_map(|l| l.strip_prefix("# public key: "))
            .unwrap()
            .to_string()
    }
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

fn write(path: &Path, content: &str) {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).unwrap();
    }
    std::fs::write(path, content).unwrap();
}

/// dan's laptop creates the repo with an encrypted .env.local and pushes it.
fn start(world: &World, laptop: &Machine, content: &str) -> PathBuf {
    let repo = laptop.clone(world, "repo");
    write(&repo.join(".env.local"), content);
    laptop.gse(&repo, &["init", "--no-agents"]);
    laptop.git(&repo, &["commit", "-q", "-m", "init"]);
    laptop.git(&repo, &["push", "-q", "origin", "main"]);
    repo
}

#[test]
fn personal_round_trip_between_machines() {
    let world = World::new();
    let laptop = world.machine("laptop", "dan");
    laptop.setup();
    let a = start(&world, &laptop, "A=1\n");

    let files = laptop.git(&a, &["ls-files"]);
    assert!(files.contains(".gse/users/dan/.env.local.age"), "{files}");
    assert!(files.contains(".gse/users/dan/recipients.txt"), "{files}");
    assert!(!files.lines().any(|l| l == ".env.local"), "{files}");
    let cipher = read(&a.join(".gse/users/dan/.env.local.age"));
    assert!(cipher.starts_with("-----BEGIN AGE ENCRYPTED FILE-----"));
    assert!(!cipher.contains("A=1"));

    let desktop = world.machine("desktop", "dan");
    desktop.setup_as(&laptop);
    let b = desktop.clone(&world, "repo");
    assert_eq!(read(&b.join(".env.local")), "A=1\n");

    // An env-only change: plain `git commit` finds nothing; `gse commit` commits it.
    write(&a.join(".env.local"), "A=2\n");
    assert!(
        !laptop
            .git_out(&a, &["commit", "-q", "-m", "x"])
            .status
            .success()
    );
    laptop.gse(&a, &["commit", "-m", "update env"]);
    laptop.git(&a, &["push", "-q", "origin", "main"]);

    desktop.git(&b, &["pull", "-q", "--no-rebase"]);
    assert_eq!(read(&b.join(".env.local")), "A=2\n");
    assert!(desktop.gse(&b, &["status"]).contains("in sync"));
}

#[test]
fn unchanged_files_keep_their_ciphertext() {
    let world = World::new();
    let laptop = world.machine("laptop", "dan");
    laptop.setup();
    let a = start(&world, &laptop, "A=1\n");
    let before = read(&a.join(".gse/users/dan/.env.local.age"));
    write(&a.join("README"), "hi\n");
    laptop.git(&a, &["add", "README"]);
    laptop.git(&a, &["commit", "-q", "-m", "readme"]);
    assert_eq!(read(&a.join(".gse/users/dan/.env.local.age")), before);
}

#[test]
fn partial_commits_keep_the_index_level_with_head() {
    let world = World::new();
    let laptop = world.machine("laptop", "dan");
    laptop.setup();
    let a = start(&world, &laptop, "A=1\n");
    write(&a.join(".env.local"), "A=2\n");
    write(&a.join("README"), "hi\n");
    laptop.git(&a, &["add", "README"]);
    write(&a.join("README"), "hello\n");
    // `git commit <path>` gives hooks a temporary index.
    laptop.git(&a, &["commit", "-q", "-m", "readme", "README"]);
    let shown = laptop.git(&a, &["show", "HEAD:.gse/users/dan/.env.local.age"]);
    assert_eq!(shown, read(&a.join(".gse/users/dan/.env.local.age")));
    let status = laptop.git(&a, &["status", "--porcelain", "--", ".gse"]);
    assert_eq!(status, "", "the index lags HEAD:\n{status}");
}

#[test]
fn pull_merges_local_edits() {
    let world = World::new();
    let laptop = world.machine("laptop", "dan");
    laptop.setup();
    let a = start(&world, &laptop, "A=1\nB=1\nC=1\n");
    let desktop = world.machine("desktop", "dan");
    desktop.setup_as(&laptop);
    let b = desktop.clone(&world, "repo");

    write(&a.join(".env.local"), "A=2\nB=1\nC=1\n");
    laptop.gse(&a, &["commit"]);
    laptop.git(&a, &["push", "-q", "origin", "main"]);

    // An unsynced local edit on another line survives the pull.
    write(&b.join(".env.local"), "A=1\nB=1\nC=3\n");
    desktop.git(&b, &["pull", "-q", "--no-rebase"]);
    assert_eq!(read(&b.join(".env.local")), "A=2\nB=1\nC=3\n");
}

fn concurrent_edits(rebase: bool) {
    let world = World::new();
    let laptop = world.machine("laptop", "dan");
    laptop.setup();
    let a = start(&world, &laptop, "A=1\nB=1\nC=1\n");
    let desktop = world.machine("desktop", "dan");
    desktop.setup_as(&laptop);
    let b = desktop.clone(&world, "repo");

    write(&a.join(".env.local"), "A=2\nB=1\nC=1\n");
    laptop.gse(&a, &["commit"]);
    laptop.git(&a, &["push", "-q", "origin", "main"]);

    write(&b.join(".env.local"), "A=1\nB=1\nC=3\n");
    desktop.gse(&b, &["commit"]);
    let flag = if rebase { "--rebase" } else { "--no-rebase" };
    desktop.git(&b, &["pull", "-q", flag]);
    assert_eq!(read(&b.join(".env.local")), "A=2\nB=1\nC=3\n");
    desktop.git(&b, &["push", "-q", "origin", "main"]);

    laptop.git(&a, &["pull", "-q", "--no-rebase"]);
    assert_eq!(read(&a.join(".env.local")), "A=2\nB=1\nC=3\n");
}

#[test]
fn concurrent_commits_merge_through_the_driver() {
    concurrent_edits(false);
}

#[test]
fn concurrent_commits_rebase_through_the_driver() {
    concurrent_edits(true);
}

#[test]
fn conflicting_edits_leave_markers_until_resolved() {
    let world = World::new();
    let laptop = world.machine("laptop", "dan");
    laptop.setup();
    let a = start(&world, &laptop, "A=1\n");
    let desktop = world.machine("desktop", "dan");
    desktop.setup_as(&laptop);
    let b = desktop.clone(&world, "repo");

    write(&a.join(".env.local"), "A=laptop\n");
    laptop.gse(&a, &["commit"]);
    laptop.git(&a, &["push", "-q", "origin", "main"]);

    write(&b.join(".env.local"), "A=desktop\n");
    desktop.gse(&b, &["commit"]);
    let out = desktop.git_out(&b, &["pull", "-q", "--no-rebase"]);
    assert!(!out.status.success(), "{}", text(&out));
    let conflicted = read(&b.join(".env.local"));
    assert!(conflicted.contains("<<<<<<< ours"), "{conflicted}");
    assert!(conflicted.contains("A=laptop") && conflicted.contains("A=desktop"));
    assert!(desktop.gse(&b, &["status"]).contains("conflict"));
    assert!(!desktop.gse_out(&b, &["sync"]).status.success());

    write(&b.join(".env.local"), "A=both\n");
    desktop.gse(&b, &["sync"]);
    desktop.git(&b, &["commit", "-q", "--no-edit"]);
    desktop.git(&b, &["push", "-q", "origin", "main"]);

    laptop.git(&a, &["pull", "-q", "--no-rebase"]);
    assert_eq!(read(&a.join(".env.local")), "A=both\n");
}

#[test]
fn staged_plaintext_blocks_the_commit() {
    let world = World::new();
    let laptop = world.machine("laptop", "dan");
    laptop.setup();
    let a = start(&world, &laptop, "A=1\n");
    laptop.git(&a, &["add", "-f", ".env.local"]);
    let out = laptop.git_out(&a, &["commit", "-q", "-m", "oops"]);
    assert!(!out.status.success(), "{}", text(&out));
    assert!(String::from_utf8_lossy(&out.stderr).contains("staged in plaintext"));
}

#[test]
fn conflict_markers_block_the_commit() {
    let world = World::new();
    let laptop = world.machine("laptop", "dan");
    laptop.setup();
    let a = start(&world, &laptop, "A=1\n");
    write(
        &a.join(".env.local"),
        "<<<<<<< ours\nA=1\n=======\nA=2\n>>>>>>> theirs\n",
    );
    write(&a.join("README"), "hi\n");
    laptop.git(&a, &["add", "README"]);
    assert!(
        !laptop
            .git_out(&a, &["commit", "-q", "-m", "x"])
            .status
            .success()
    );
}

#[test]
fn init_writes_managed_files_once() {
    let world = World::new();
    let laptop = world.machine("laptop", "dan");
    laptop.setup();
    let repo = laptop.clone(&world, "repo");
    write(&repo.join(".env.local"), "A=1\n");
    write(&repo.join(".env.example"), "A=\n");
    laptop.gse(&repo, &["init"]);
    let agents = read(&repo.join("AGENTS.md"));
    assert!(agents.contains("<!-- gse:start"));
    assert!(agents.contains("https://github.com/danthegoodman1/git-secure-env"));
    assert!(read(&repo.join(".gse/README.md")).contains("Working here (humans and agents)"));
    assert!(read(&repo.join(".gitignore")).contains("/.env.local"));
    let cfg = read(&repo.join(".gse/config.toml"));
    assert!(
        cfg.contains("path = \".env.local\"") && !cfg.contains(".env.example"),
        "{cfg}"
    );

    laptop.gse(&repo, &["init"]);
    assert_eq!(read(&repo.join("AGENTS.md")), agents);
    assert_eq!(
        read(&repo.join(".gitignore"))
            .matches("/.env.local")
            .count(),
        1
    );
}

#[test]
fn shims_chain_to_repository_hooks() {
    let world = World::new();
    let laptop = world.machine("laptop", "dan");
    laptop.gse(&laptop.root, &["install", "--mode", "shims"]);
    laptop.gse(&laptop.root, &["keygen", "--handle", "dan"]);
    let repo = laptop.clone(&world, "repo");
    write(
        &repo.join(".git/hooks/commit-msg"),
        "#!/bin/sh\necho ran > \"$(git rev-parse --git-dir)/commit-msg-ran\"\n",
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            repo.join(".git/hooks/commit-msg"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    write(&repo.join(".env.local"), "A=1\n");
    laptop.gse(&repo, &["init", "--no-agents"]);
    laptop.git(&repo, &["commit", "-q", "-m", "init"]);
    assert!(repo.join(".git/commit-msg-ran").exists());
    assert!(
        laptop
            .git(&repo, &["ls-files"])
            .contains(".gse/users/dan/.env.local.age")
    );

    laptop.gse(&laptop.root, &["uninstall"]);
    let hooks_path = laptop.git_out(
        &laptop.root,
        &["config", "--global", "--get", "core.hooksPath"],
    );
    assert!(!hooks_path.status.success());
}

#[test]
fn shared_scope_with_a_ci_key() {
    let world = World::new();
    let laptop = world.machine("laptop", "dan");
    laptop.setup();
    let a = start(&world, &laptop, "A=1\n");
    write(&a.join(".env.ci"), "TOKEN=ci-secret\n");
    laptop.gse(&a, &["track", ".env.ci", "--scope", "ci"]);
    let secret = laptop.gse(
        &a,
        &["grant", "--new-key", "github-actions", "--scope", "ci"],
    );
    let secret = secret.trim().to_string();
    assert!(secret.starts_with("AGE-SECRET-KEY-"), "{secret}");
    let mirror = read(&a.join(".gse/shared/ci/recipients.txt"));
    assert!(mirror.contains("# @key:github-actions"), "{mirror}");
    assert!(mirror.contains("# @key:dan"), "{mirror}");
    laptop.git(&a, &["commit", "-q", "-m", "ci env"]);
    laptop.git(&a, &["push", "-q", "origin", "main"]);

    // CI: no hooks, no gse identity, only GSE_IDENTITY.
    let ci = world.machine("ci", "runner");
    let c = ci.clone(&world, "repo");
    assert!(!c.join(".env.ci").exists());
    let env = [("GSE_IDENTITY", secret.as_str())];
    let out = ci.run_raw(&gse_bin(), &c, &["decrypt", "--scope", "ci"], &env);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(read(&c.join(".env.ci")), "TOKEN=ci-secret\n");
    assert!(!c.join(".env.local").exists());

    // Revoking re-encrypts: the old CI key cannot read new versions.
    laptop.gse(&a, &["revoke", "github-actions", "--scope", "ci"]);
    laptop.git(&a, &["commit", "-q", "-m", "revoke ci"]);
    laptop.git(&a, &["push", "-q", "origin", "main"]);
    ci.git(&c, &["pull", "-q", "--no-rebase"]);
    let out = ci.run_raw(
        &gse_bin(),
        &c,
        &["decrypt", "--scope", "ci", "--force"],
        &env,
    );
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("decrypted nothing"),
        "{}",
        text(&out)
    );
}

#[cfg(unix)]
#[test]
fn stacks_layer_personal_over_shared() {
    let world = World::new();
    let laptop = world.machine("laptop", "dan");
    laptop.setup();
    let a = start(&world, &laptop, "A=2\nDB=postgres://me\n");
    write(&a.join(".env"), "A=1\nB=1\nDB= #personal\nAWS= #personal\n");
    laptop.gse(&a, &["track", ".env", "--scope", "team"]);
    let out = laptop.gse_out(&a, &["export", "--format", "json"]);
    assert!(out.status.success(), "{}", text(&out));
    let json = String::from_utf8_lossy(&out.stdout);
    assert!(json.contains("\"A\": \"2\""), "{json}");
    assert!(json.contains("\"B\": \"1\""), "{json}");
    assert!(json.contains("\"DB\": \"postgres://me\""), "{json}");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("AWS"),
        "{}",
        text(&out)
    );
    let got = laptop.gse(&a, &["run", "--", "sh", "-c", "printf %s \"$DB-$B\""]);
    assert_eq!(got, "postgres://me-1");

    // A #personal key given a value in the shared file draws a warning.
    write(&a.join(".env"), "A=1\nDB=postgres://mine #personal\n");
    let out = laptop.gse_out(&a, &["commit"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("gives #personal keys a value: DB"),
        "{}",
        text(&out)
    );
}

#[test]
fn textconv_shows_plaintext_diffs() {
    let world = World::new();
    let laptop = world.machine("laptop", "dan");
    laptop.setup();
    let a = start(&world, &laptop, "A=1\n");
    write(&a.join(".env.local"), "A=2\n");
    laptop.gse(&a, &["commit"]);
    let log = laptop.git(
        &a,
        &[
            "-c",
            "core.pager=cat",
            "log",
            "-p",
            "-1",
            "--",
            ".gse/users",
        ],
    );
    assert!(log.contains("-A=1") && log.contains("+A=2"), "{log}");
}

#[test]
fn doctor_self_test_passes() {
    let world = World::new();
    let laptop = world.machine("laptop", "dan");
    laptop.setup();
    let out = laptop.gse_out(&laptop.root, &["doctor"]);
    let report = text(&out);
    assert!(out.status.success(), "{report}");
    assert!(report.contains("self-test"), "{report}");
}

#[test]
fn doctor_fails_without_hooks() {
    let world = World::new();
    let laptop = world.machine("laptop", "dan");
    laptop.gse(&laptop.root, &["keygen", "--handle", "dan"]);
    let out = laptop.gse_out(&laptop.root, &["doctor"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("run `gse install`"));
}

#[test]
fn outsiders_edit_of_recipients_is_ignored() {
    let world = World::new();
    let laptop = world.machine("laptop", "dan");
    laptop.setup();
    let a = start(&world, &laptop, "A=1\n");
    write(&a.join(".env.team"), "T=1\n");
    laptop.gse(&a, &["track", ".env.team", "--scope", "team"]);
    laptop.git(&a, &["commit", "-q", "-m", "team"]);
    laptop.git(&a, &["push", "-q", "origin", "main"]);

    // mallory can push but cannot read the team scope; she adds her key to the mirror.
    let mallory = world.machine("mallory", "mallory");
    mallory.setup();
    let m = mallory.clone(&world, "repo");
    assert!(!m.join(".env.team").exists());
    let mirror = m.join(".gse/shared/team/recipients.txt");
    let mut text_ = read(&mirror);
    text_.push_str(&format!("\n# @key:mallory\n{}\n", mallory.public_key()));
    write(&mirror, &text_);
    mallory.git(&m, &["add", ".gse/shared/team/recipients.txt"]);
    mallory.git(&m, &["commit", "-q", "-m", "add me"]);
    mallory.git(&m, &["push", "-q", "origin", "main"]);

    laptop.git(&a, &["pull", "-q", "--no-rebase"]);
    write(&a.join(".env.team"), "T=2\n");
    let out = laptop.gse_out(&a, &["commit"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("restored it"),
        "{}",
        text(&out)
    );
    assert!(!read(&a.join(".gse/shared/team/recipients.txt")).contains("mallory"));
    laptop.git(&a, &["push", "-q", "origin", "main"]);

    mallory.git(&m, &["pull", "-q", "--no-rebase"]);
    assert!(!m.join(".env.team").exists());
    assert!(
        mallory
            .gse(&m, &["status"])
            .contains("not a recipient of team")
    );
}

#[test]
fn a_taken_handle_is_refused() {
    let world = World::new();
    let laptop = world.machine("laptop", "dan");
    laptop.setup();
    let _a = start(&world, &laptop, "A=1\n");

    // Another person picks the same handle.
    let other = world.machine("other", "dan");
    other.setup();
    let o = other.clone(&world, "repo");
    let before = read(&o.join(".gse/users/dan/.env.local.age"));
    write(&o.join(".env.local"), "MINE=1\n");
    write(&o.join("README"), "hi\n");
    other.git(&o, &["add", "README"]);
    let out = other.git_out(&o, &["commit", "-q", "-m", "x"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("belongs to someone else"),
        "{}",
        text(&out)
    );
    assert_eq!(read(&o.join(".gse/users/dan/.env.local.age")), before);
}

#[test]
fn untrack_removes_encrypted_copies() {
    let world = World::new();
    let laptop = world.machine("laptop", "dan");
    laptop.setup();
    let a = start(&world, &laptop, "A=1\n");
    laptop.gse(&a, &["untrack", ".env.local"]);
    laptop.git(&a, &["commit", "-q", "-m", "untrack"]);
    assert!(!laptop.git(&a, &["ls-files"]).contains(".env.local.age"));
    assert_eq!(read(&a.join(".env.local")), "A=1\n");
}

fn ssh_keygen_available() -> bool {
    Command::new("ssh-keygen")
        .arg("-?")
        .output()
        .map(|_| true)
        .unwrap_or(false)
}

#[test]
fn grant_a_github_user_who_decrypts_with_an_ssh_key() {
    if !ssh_keygen_available() {
        eprintln!("skipping: ssh-keygen is not installed");
        return;
    }
    let world = World::new();
    let laptop = world.machine("laptop", "dan");
    laptop.setup();
    let a = start(&world, &laptop, "A=1\n");

    let alice = world.machine("alice", "alice");
    let ssh = alice.home.join(".ssh");
    std::fs::create_dir_all(&ssh).unwrap();
    let out = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-C", "alice@laptop", "-f"])
        .arg(ssh.join("id_ed25519"))
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out));
    let public = read(&ssh.join("id_ed25519.pub"));
    write(
        &world.dir.path().join("github/alice.keys"),
        &format!(
            "{}ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBOx\n",
            public
        ),
    );

    write(&a.join(".env.team"), "T=1\n");
    laptop.gse(&a, &["track", ".env.team", "--scope", "team"]);
    laptop.gse(&a, &["grant", "alice", "--scope", "team"]);
    let mirror = read(&a.join(".gse/shared/team/recipients.txt"));
    assert!(mirror.contains("# @github:alice"), "{mirror}");
    assert!(
        mirror.contains("# unsupported by age: ecdsa-sha2-nistp256"),
        "{mirror}"
    );
    laptop.git(&a, &["commit", "-q", "-m", "grant alice"]);
    laptop.git(&a, &["push", "-q", "origin", "main"]);

    // alice has hooks but no gse identity: her SSH key opens the team scope.
    alice.gse(&alice.root, &["install"]);
    let b = alice.clone(&world, "repo");
    assert_eq!(read(&b.join(".env.team")), "T=1\n");
    assert!(!b.join(".env.local").exists());

    // As a member she can change the file; dan receives it.
    write(&b.join(".env.team"), "T=2\n");
    alice.gse(&b, &["commit"]);
    alice.git(&b, &["push", "-q", "origin", "main"]);
    laptop.git(&a, &["pull", "-q", "--no-rebase"]);
    assert_eq!(read(&a.join(".env.team")), "T=2\n");
}
