//! gse (git-secure-env): commit `.env*` files to git encrypted.

mod access;
mod config;
mod crypto;
mod dotenv;
mod git;
mod github;
mod hooks;
mod identity;
mod merge;
mod payload;
mod recipients;
mod setup;
mod state;
mod sync;
mod templates;
mod ui;
mod util;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "gse",
    version,
    about = "Commit .env files to git, encrypted",
    long_about = "gse (git-secure-env) keeps plaintext env files gitignored and commits encrypted \
copies under .gse/. Global git hooks encrypt them on commit and decrypt them on pull, checkout, \
and rebase.\n\nStart with `gse install`, `gse keygen`, then `gse init` in a repository.\n\
https://github.com/danthegoodman1/git-secure-env"
)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Install global git hooks, the merge driver, and the diff textconv
    Install {
        /// Hook mechanism; defaults to config hooks on git 2.54+, else shims
        #[arg(long, value_enum)]
        mode: Option<hooks::HookMode>,
    },
    /// Remove gse's global hooks and drivers
    Uninstall,
    /// Check the installation, identity, and repository; run a round-trip self-test
    Doctor {
        /// Skip the self-test
        #[arg(long)]
        no_self_test: bool,
    },
    /// Create or import your identity and set your handle
    Keygen {
        /// Your directory name in .gse/users/ (default: $USER)
        #[arg(long)]
        handle: Option<String>,
        /// Also encrypt personal files to this GitHub user's SSH keys
        #[arg(long)]
        github: Option<String>,
        /// Import an identity from a file (or `-` for stdin) instead of generating one
        #[arg(long, value_name = "FILE")]
        import: Option<String>,
        /// Replace an existing identity
        #[arg(long)]
        force: bool,
    },
    /// Set up .gse/ in this repository, or refresh its managed files
    Init {
        /// Track nothing yet
        #[arg(long)]
        no_track: bool,
        /// Leave AGENTS.md and CLAUDE.md alone
        #[arg(long)]
        no_agents: bool,
    },
    /// Track env files matching a pattern (relative to the repository root)
    Track {
        pattern: String,
        /// `user` (a copy per developer) or a shared scope such as `team` or `ci`
        #[arg(long, default_value = "user")]
        scope: String,
    },
    /// Stop tracking a pattern and remove its encrypted copies
    Untrack { pattern: String },
    /// Show each tracked file's scope and sync state
    Status,
    /// Decrypt incoming changes, then encrypt and stage local ones
    Sync,
    /// Sync, then commit only .gse/
    Commit {
        #[arg(short, long)]
        message: Option<String>,
    },
    /// Give GitHub users or keys access to a shared scope
    Grant {
        /// GitHub usernames whose SSH keys to add
        users: Vec<String>,
        #[arg(long)]
        scope: String,
        /// A public key to add (age1… or ssh-ed25519/ssh-rsa)
        #[arg(long)]
        key: Option<String>,
        /// The owner name for --key
        #[arg(long)]
        name: Option<String>,
        /// Generate a key pair for this name (for CI) and print its secret key
        #[arg(long, value_name = "NAME")]
        new_key: Option<String>,
    },
    /// Remove an owner's keys from a shared scope
    Revoke {
        /// Owner name: a GitHub username or key name
        name: String,
        #[arg(long)]
        scope: String,
    },
    /// Re-fetch GitHub keys for shared scopes, or with --me your own
    Refresh {
        #[arg(long)]
        scope: Option<String>,
        #[arg(long)]
        me: bool,
    },
    /// Run a command with a stack's layered environment
    Run {
        #[arg(long, default_value = "default")]
        stack: String,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        command: Vec<String>,
    },
    /// Print a stack's layered environment
    Export {
        #[arg(long, default_value = "default")]
        stack: String,
        #[arg(long, value_enum, default_value = "dotenv")]
        format: dotenv::ExportFormat,
    },
    /// Write plaintext files from .gse/ (for CI and recovery)
    Decrypt {
        /// Only this scope (`user` or a shared scope name)
        #[arg(long)]
        scope: Option<String>,
        /// Overwrite existing plaintext files
        #[arg(long)]
        force: bool,
    },
    /// Hook entry point (plumbing)
    Hook {
        event: String,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Git merge driver for .age files (plumbing)
    MergeDriver {
        base: PathBuf,
        ours: PathBuf,
        theirs: PathBuf,
        path: String,
    },
    /// Decrypt a file for git diff (plumbing)
    Textconv { file: PathBuf },
}

fn code(n: i32) -> ExitCode {
    ExitCode::from(u8::try_from(n.clamp(0, 255)).unwrap_or(1))
}

fn with_ctx<T>(f: impl FnOnce(&sync::Ctx) -> Result<T>) -> Result<T> {
    let repo = git::Repo::discover()?.context("run this inside a git repository")?;
    let ctx = sync::Ctx::open(repo)?
        .context("this repository has no .gse/config.toml; run `gse init`")?;
    f(&ctx)
}

fn run(cli: Cli) -> Result<i32> {
    match cli.command {
        Cmd::Install { mode } => hooks::install(mode).map(|_| 0),
        Cmd::Uninstall => hooks::uninstall().map(|_| 0),
        Cmd::Doctor { no_self_test } => {
            hooks::doctor(!no_self_test).map(|ok| if ok { 0 } else { 1 })
        }
        Cmd::Keygen {
            handle,
            github,
            import,
            force,
        } => setup::keygen(setup::KeygenArgs {
            handle,
            github,
            import,
            force,
        })
        .map(|_| 0),
        Cmd::Init {
            no_track,
            no_agents,
        } => setup::init(setup::InitArgs {
            no_track,
            no_agents,
        })
        .map(|_| 0),
        Cmd::Track { pattern, scope } => setup::track(&pattern, &scope).map(|_| 0),
        Cmd::Untrack { pattern } => setup::untrack(&pattern).map(|_| 0),
        Cmd::Status => setup::status().map(|_| 0),
        Cmd::Sync => setup::sync(),
        Cmd::Commit { message } => setup::commit(message),
        Cmd::Grant {
            users,
            scope,
            key,
            name,
            new_key,
        } => with_ctx(|ctx| {
            access::grant(
                ctx,
                access::GrantArgs {
                    scope,
                    users,
                    key,
                    name,
                    new_key,
                },
            )
        })
        .map(|_| 0),
        Cmd::Revoke { name, scope } => {
            with_ctx(|ctx| access::revoke(ctx, &scope, &name)).map(|_| 0)
        }
        Cmd::Refresh { scope, me } => {
            if me {
                access::refresh_me()?;
                if let Some(repo) = git::Repo::discover()?
                    && let Some(ctx) = sync::Ctx::open(repo)?
                {
                    ctx.sync()?;
                }
                return Ok(0);
            }
            with_ctx(|ctx| {
                let scopes = match scope {
                    Some(s) => vec![s],
                    None => ctx.shared_scopes(),
                };
                if scopes.is_empty() {
                    ui::info("no shared scopes here; `gse refresh --me` refreshes your own keys");
                }
                for s in scopes {
                    access::refresh_scope(ctx, &s)?;
                }
                Ok(0)
            })
        }
        Cmd::Run { stack, command } => dotenv::run(&stack, &command),
        Cmd::Export { stack, format } => {
            let layered = dotenv::resolve(&stack)?;
            print!("{}", dotenv::render(&layered.vars, format));
            Ok(0)
        }
        Cmd::Decrypt { scope, force } => setup::decrypt(scope, force).map(|_| 0),
        Cmd::Hook { event, args } => Ok(hooks::run_hook(&event, &args)),
        Cmd::MergeDriver {
            base,
            ours,
            theirs,
            path,
        } => merge::driver(&base, &ours, &theirs, &path),
        Cmd::Textconv { file } => merge::textconv(&file).map(|_| 0),
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(n) => code(n),
        Err(err) => {
            ui::error(format!("{err:#}"));
            ExitCode::FAILURE
        }
    }
}
