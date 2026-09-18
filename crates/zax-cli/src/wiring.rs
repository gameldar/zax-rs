//! The only place that knows the whole dependency graph.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use zax_app::{FileSystem, PortError};
use zax_config::Config;
use zax_domain::RootRepoName;
use zax_exec::{SystemExec, SystemHandover};
use zax_infra_git::GitCli;
use zax_infra_github::GhCli;
use zax_infra_tmux::{conf, TmuxCli, SOCKET_DEFAULT};

pub type Git = GitCli<SystemExec>;
pub type Gh = GhCli<SystemExec>;
pub type Tmux = TmuxCli<SystemExec, SystemHandover>;

pub struct Resolved {
    pub cwd: PathBuf,
    pub git: Git,
    pub gh: Gh,
    pub repo: RootRepoName,
    pub config: Config,
}

pub fn home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is not set")
}

/// `$XDG_CONFIG_HOME`, or `~/.config`.
pub fn config_home() -> Result<PathBuf> {
    match std::env::var_os("XDG_CONFIG_HOME") {
        Some(value) if !value.is_empty() => Ok(PathBuf::from(value)),
        _ => Ok(home()?.join(".config")),
    }
}

/// Where the rendered tmux.conf lives. It is regenerated on every
/// `session start`, so a cache directory is the right home for it.
pub fn runtime_dir() -> Result<PathBuf> {
    match std::env::var_os("XDG_CACHE_HOME") {
        Some(value) if !value.is_empty() => Ok(PathBuf::from(value).join("zax")),
        _ => Ok(home()?.join(".cache").join("zax")),
    }
}

/// Resolve everything that depends on the current directory.
pub fn resolve() -> Result<Resolved> {
    let cwd = std::env::current_dir().context("could not read the current directory")?;
    let git = GitCli::new(SystemExec, cwd.clone());
    let gh = GhCli::new(SystemExec, cwd.clone());

    let repo = git
        .repo_name()
        .context("this directory is not inside a git repository")?;
    let main_worktree = {
        use zax_app::GitRepo;
        git.main_worktree()?
    };

    let home = home()?;
    let config = zax_config::load(
        &config_home()?,
        &main_worktree,
        &repo,
        &home.to_string_lossy(),
    )?;

    Ok(Resolved {
        cwd,
        git,
        gh,
        repo,
        config,
    })
}

/// A tmux adapter configured with the rendered conf for this repo's prefix.
///
/// The conf is rewritten on every command, but tmux consults `-f` only when it
/// starts a server and every zax session shares one socket, so this repo's
/// prefix only takes effect if this is the session that starts the server.
/// The prefix is global to the zax server by design; see `conf::write_to`.
pub fn tmux(config: &Config) -> Result<Tmux> {
    let conf_path = conf::write_to(&runtime_dir()?, &config.tmux.prefix)
        .context("could not write zax's tmux configuration")?;
    Ok(TmuxCli::new(
        SystemExec,
        SystemHandover,
        SOCKET_DEFAULT.to_owned(),
        conf_path,
    ))
}

/// The absolute path of the running binary, so panes re-invoke this build.
pub fn zax_bin() -> Result<PathBuf> {
    std::env::current_exe().context("could not find the running zax binary")
}

/// `FileSystem` over the real one.
pub struct RealFs;

impl FileSystem for RealFs {
    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn create_dir_all(&self, path: &Path) -> Result<(), PortError> {
        std::fs::create_dir_all(path).map_err(|e| PortError::Io {
            operation: "create directory",
            path: path.to_owned(),
            detail: e.to_string(),
        })
    }

    fn copy_file(&self, from: &Path, to: &Path) -> Result<(), PortError> {
        std::fs::copy(from, to)
            .map(|_| ())
            .map_err(|e| PortError::Io {
                operation: "copy",
                path: from.to_owned(),
                detail: e.to_string(),
            })
    }
}

/// `ProcessRunner` over the real one, inheriting stdio so a hook's output is
/// visible while it runs.
pub struct RealRunner;

impl zax_app::ProcessRunner for RealRunner {
    fn run(
        &self,
        cwd: &Path,
        command: &zax_domain::Command,
        env: &[(String, String)],
    ) -> Result<String, PortError> {
        let program = command.program();
        let rest: Vec<String> = command.argv().iter().skip(1).cloned().collect();
        let status = std::process::Command::new(program)
            .args(&rest)
            .current_dir(cwd)
            .envs(env.iter().cloned())
            .status()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    PortError::MissingTool { tool: "hook" }
                } else {
                    PortError::Io {
                        operation: "run hook",
                        path: cwd.to_owned(),
                        detail: e.to_string(),
                    }
                }
            })?;

        if status.success() {
            Ok(String::new())
        } else {
            Err(PortError::CommandFailed {
                tool: "hook",
                status: status.code().unwrap_or(-1),
                stderr: format!("`{program}` exited with a non-zero status"),
            })
        }
    }
}
