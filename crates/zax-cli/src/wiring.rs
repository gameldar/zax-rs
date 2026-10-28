//! The only place that knows the whole dependency graph.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use zax_app::{Editor, FileSystem, PortError, WorktreeOpener};
use zax_config::{Config, EditorConfig, OpenConfig};
use zax_domain::{MainCheckout, RootRepoName, TodoFile};
use zax_exec::{InteractiveExec, SystemExec, SystemHandover};
use zax_infra_editor::EditorCli;
use zax_infra_git::{GitCheckouts, GitCli};
use zax_infra_github::GhCli;
use zax_infra_herdr::{HerdrCli, HerdrEditor};
use zax_infra_ssh::SshAddCli;
use zax_infra_tmux::{conf, TmuxCli, SOCKET_DEFAULT};

pub type Git = GitCli<SystemExec>;
pub type Gh = GhCli<SystemExec>;
pub type Tmux = TmuxCli<SystemExec, SystemHandover>;
pub type SshAdapter = SshAddCli<SystemExec>;

/// What `resolve` found about the repository the cwd is in, when it is in
/// one at all.
pub struct RepoContext {
    pub git: Git,
    pub name: RootRepoName,
    pub main_worktree: PathBuf,
}

pub struct Resolved {
    pub cwd: PathBuf,
    pub gh: Gh,
    /// `None` outside a git repository, which `projects.root` makes a legal
    /// place to run zax from. A command that cannot do without a repository
    /// asks for one through `require_repo`.
    pub repo: Option<RepoContext>,
    pub config: Config,
    pub config_home: PathBuf,
    pub home: PathBuf,
}

impl Resolved {
    /// The repository a command needs but this session was not run inside.
    pub fn require_repo(&self) -> Result<&RepoContext> {
        self.repo
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("this needs a git repository; run it inside one"))
    }

    /// The cwd repository's main worktree, for `ResolveCheckout`'s rule that
    /// the current checkout wins outright before any candidate is looked
    /// at. `None` outside a repository, where there is no current checkout
    /// to prefer.
    pub fn here(&self) -> Option<&Path> {
        self.repo.as_ref().map(|repo| repo.main_worktree.as_path())
    }

    /// Where a relative tickets provider path is anchored: the main
    /// worktree inside a repository, or the global config directory outside
    /// one, since that is the one directory a repo-less session is still
    /// tied to.
    pub fn provider_anchor(&self) -> PathBuf {
        match &self.repo {
            Some(repo) => repo.main_worktree.clone(),
            None => self.config_home.join("zax"),
        }
    }
}

/// `Ok` when the global config has turned `projects` on; otherwise the one
/// refusal for a directory that is neither a git repository nor opted into
/// working outside one.
///
/// `cause` is git's own answer to the repository query, kept in the chain
/// rather than replaced by the hint: "not a git repository" is not the only
/// reason the query can fail, and whichever it was is the first thing the
/// reader needs.
///
/// The hint names what `projects.root` actually buys - a session outside a
/// repository - because the repository-only commands reach this too, and
/// setting it does not make `worktree list` or `doctor` work.
pub fn outside_repository(global: &Config, cause: PortError) -> Result<()> {
    if global.projects.is_some() {
        return Ok(());
    }
    Err(cause).context(
        "this directory is not inside a git repository\n\
         set projects.root in ~/.config/zax/config.toml to start a session outside a repository",
    )
}

pub fn home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is not set")
}

/// `$XDG_DATA_HOME`, or `~/.local/share`.
pub fn data_home() -> Result<PathBuf> {
    Ok(data_home_from(std::env::var_os("XDG_DATA_HOME"), &home()?))
}

/// The XDG base-directory spec says a relative value is invalid and is to be
/// ignored, as is an empty one.
fn data_home_from(value: Option<std::ffi::OsString>, home: &Path) -> PathBuf {
    match value.map(PathBuf::from) {
        Some(path) if path.is_absolute() => path,
        _ => home.join(".local").join("share"),
    }
}

/// The todo file config names, or `todo.md` under the data home.
pub fn todo_file(config: &Config) -> Result<TodoFile> {
    match &config.todo.file {
        Some(file) => Ok(file.clone()),
        None => {
            TodoFile::new(data_home()?.join("zax").join("todo.md")).map_err(|e| anyhow::anyhow!(e))
        }
    }
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

/// Resolve everything that depends on the current directory. Outside a git
/// repository this still succeeds, provided the global config has turned
/// `projects` on: a zax session is often panes alone, tied to no
/// repository, and it is each command's own job to ask for one, through
/// `Resolved::require_repo`, when it cannot do without it.
pub fn resolve() -> Result<Resolved> {
    let cwd = std::env::current_dir().context("could not read the current directory")?;
    let git = GitCli::new(SystemExec, cwd.clone());
    let gh = GhCli::new(SystemExec, cwd.clone());
    let home = home()?;
    let config_home = config_home()?;

    let repo_name = match git.repo_name() {
        Ok(name) => Ok(name),
        // Only git saying there is no repository here opens the repo-less
        // path. Any other failure is a repository zax could not read - no
        // git on PATH, a `safe.directory` refusal, a corrupt `.git` - and
        // reporting it as an absent repository would leave the user with a
        // session rooted at the cwd and no word of what was ignored.
        Err(error) if zax_infra_git::is_outside_repository(&error) => Err(error),
        Err(error) => return Err(error.into()),
    };

    let (repo, config) = match repo_name {
        Ok(name) => {
            let main_worktree = {
                use zax_app::GitRepo;
                git.main_worktree()?
            };
            let config =
                zax_config::load(&config_home, &main_worktree, &name, &home.to_string_lossy())?;
            (
                Some(RepoContext {
                    git,
                    name,
                    main_worktree,
                }),
                config,
            )
        }
        Err(cause) => {
            let global = zax_config::load_global(&config_home, &home.to_string_lossy())?;
            outside_repository(&global, cause)?;
            (None, global)
        }
    };

    Ok(Resolved {
        cwd,
        gh,
        repo,
        config,
        config_home,
        home,
    })
}

/// The real `CheckoutInspector`, for `ResolveCheckout`.
pub fn checkouts() -> GitCheckouts<SystemExec> {
    GitCheckouts::new(SystemExec)
}

/// What a command works against once it knows which repository that is: the
/// cwd repository, or a checkout `ResolveCheckout` found elsewhere. Carrying
/// its own `git`/`gh` and config is what lets a command run against a
/// repository that is not the cwd's without building adapters itself.
pub struct Target {
    pub git: Git,
    pub gh: Gh,
    /// The target repository's own config chain: its `worktree.copy` and
    /// `worktree.hooks`, not `worktree.root`, which stays the session's.
    pub provisioning_config: Config,
}

/// `checkout` is `ResolveCheckout`'s verdict: `None` means the cwd repository,
/// today's single-repository behaviour, so its `git`/`gh` and `ctx.config`
/// are reused as they always were; `Some` means another repository, read
/// through its own adapters and its own config chain.
pub fn target_at(ctx: &Resolved, checkout: Option<&MainCheckout>) -> Result<Target> {
    let Some(checkout) = checkout else {
        ctx.require_repo()?;
        return Ok(Target {
            git: GitCli::new(SystemExec, ctx.cwd.clone()),
            gh: GhCli::new(SystemExec, ctx.cwd.clone()),
            provisioning_config: ctx.config.clone(),
        });
    };

    let path = checkout.path().to_owned();
    let git = GitCli::new(SystemExec, path.clone());
    let gh = GhCli::new(SystemExec, path.clone());
    let repo_name = git.repo_name()?;
    let provisioning_config = zax_config::load(
        &ctx.config_home,
        &path,
        &repo_name,
        &ctx.home.to_string_lossy(),
    )?;

    Ok(Target {
        git,
        gh,
        provisioning_config,
    })
}

/// A tmux adapter configured with the rendered conf for this repo's prefix.
///
/// The conf is rewritten on every command, but tmux consults `-f` only when it
/// starts a server and every zax session shares one socket, so this repo's
/// prefix only takes effect if this is the session that starts the server.
/// The prefix is global to the zax server by design; see `conf::write_to`.
pub fn tmux(config: &Config) -> Result<Tmux> {
    let conf_path = conf::write_to(&runtime_dir()?, &config.tmux.prefix, &zax_bin()?)
        .context("could not write zax's tmux configuration")?;
    Ok(TmuxCli::new(
        SystemExec,
        SystemHandover,
        SOCKET_DEFAULT.to_owned(),
        conf_path,
    ))
}

/// The ssh agent, which zax only ever asks what it is holding.
pub fn ssh() -> SshAdapter {
    SshAddCli::new(SystemExec)
}

/// What `[open]` chose to open a worktree in.
pub enum OpenerAdapter {
    /// Not on `SystemExec`: an editor is handed to the user rather than
    /// questioned by zax, so it keeps whatever way of asking it came with.
    Command(EditorCli<InteractiveExec>),
    Herdr(HerdrCli<SystemExec>),
}

impl WorktreeOpener for OpenerAdapter {
    fn open_worktree(&self, path: &Path) -> Result<(), PortError> {
        match self {
            Self::Command(cli) => cli.open_worktree(path),
            Self::Herdr(herdr) => herdr.open_worktree(path),
        }
    }

    fn open_for_review(&self, path: &Path, pr_url: &str) -> Result<(), PortError> {
        match self {
            Self::Command(cli) => cli.open_for_review(path, pr_url),
            Self::Herdr(herdr) => herdr.open_for_review(path, pr_url),
        }
    }

    fn probe(&self) -> Result<String, PortError> {
        match self {
            Self::Command(cli) => WorktreeOpener::probe(cli),
            Self::Herdr(herdr) => WorktreeOpener::probe(herdr),
        }
    }
}

/// What `[editor]` chose to edit a file in.
pub enum EditorAdapter {
    Command(EditorCli<InteractiveExec>),
    Herdr(HerdrEditor<SystemExec>),
}

impl Editor for EditorAdapter {
    fn edit(&self, file: &Path) -> Result<(), PortError> {
        match self {
            Self::Command(cli) => cli.edit(file),
            Self::Herdr(herdr) => herdr.edit(file),
        }
    }

    fn probe(&self) -> Result<String, PortError> {
        match self {
            Self::Command(cli) => Editor::probe(cli),
            Self::Herdr(herdr) => Editor::probe(herdr),
        }
    }
}

pub fn opener(config: &Config) -> OpenerAdapter {
    match &config.open {
        OpenConfig::Command(command) => OpenerAdapter::Command(EditorCli::new(
            InteractiveExec,
            command.clone(),
            "open.command",
        )),
        OpenConfig::Herdr => OpenerAdapter::Herdr(herdr()),
    }
}

pub fn editor(config: &Config) -> EditorAdapter {
    match &config.editor {
        EditorConfig::Command(command) => EditorAdapter::Command(EditorCli::new(
            InteractiveExec,
            command.clone(),
            "editor.command",
        )),
        EditorConfig::Herdr(command) => {
            EditorAdapter::Herdr(HerdrEditor::new(herdr(), command.clone()))
        }
    }
}

fn herdr() -> HerdrCli<SystemExec> {
    HerdrCli::new(
        SystemExec,
        herdr_program_from(std::env::var_os("HERDR_BIN_PATH")),
    )
}

/// herdr gives its own panes `HERDR_BIN_PATH`, the binary of the server they
/// run under. A pane beside herdr has no such variable and finds it on PATH.
fn herdr_program_from(value: Option<std::ffi::OsString>) -> String {
    match value {
        Some(path) if !path.is_empty() => path.to_string_lossy().into_owned(),
        _ => "herdr".to_owned(),
    }
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn an_absolute_data_home_is_used() {
        assert_eq!(
            data_home_from(Some("/x/data".into()), Path::new("/h")),
            PathBuf::from("/x/data")
        );
    }

    #[test]
    fn an_unset_empty_or_relative_data_home_falls_back_to_the_home() {
        for value in [None, Some("".into()), Some("data".into())] {
            assert_eq!(
                data_home_from(value, Path::new("/h")),
                PathBuf::from("/h/.local/share")
            );
        }
    }

    fn config(open: OpenConfig, editor: EditorConfig) -> Config {
        let mut config =
            zax_config::parse::from_raw(zax_config::raw::RawConfig::default(), "/h").unwrap();
        config.open = open;
        config.editor = editor;
        config
    }

    fn nvim() -> zax_domain::Command {
        zax_domain::Command::new(vec!["nvim".to_owned()]).unwrap()
    }

    #[test]
    fn herdr_config_wires_the_herdr_adapters() {
        let config = config(OpenConfig::Herdr, EditorConfig::Herdr(nvim()));
        assert!(matches!(opener(&config), OpenerAdapter::Herdr(_)));
        assert!(matches!(editor(&config), EditorAdapter::Herdr(_)));
    }

    #[test]
    fn command_config_wires_the_command_adapters() {
        let config = config(
            OpenConfig::Command(None),
            EditorConfig::Command(Some(nvim())),
        );
        assert!(matches!(opener(&config), OpenerAdapter::Command(_)));
        assert!(matches!(editor(&config), EditorAdapter::Command(_)));
    }

    #[test]
    fn herdr_is_the_binary_herdr_names_when_it_names_one() {
        assert_eq!(herdr_program_from(Some("/opt/herdr".into())), "/opt/herdr");
        assert_eq!(herdr_program_from(Some("".into())), "herdr");
        assert_eq!(herdr_program_from(None), "herdr");
    }

    fn global_config(projects: Option<zax_config::raw::RawProjects>) -> Config {
        let raw = zax_config::raw::RawConfig {
            projects,
            ..Default::default()
        };
        zax_config::parse::from_raw(raw, "/h").unwrap()
    }

    fn no_repository_here() -> PortError {
        PortError::CommandFailed {
            tool: "git",
            status: 128,
            stderr: "fatal: not a git repository (or any of the parent directories): .git"
                .to_owned(),
        }
    }

    #[test]
    fn outside_a_repository_with_projects_is_allowed() {
        let global = global_config(Some(zax_config::raw::RawProjects {
            root: Some("/h/src".to_owned()),
            paths: None,
        }));
        assert!(outside_repository(&global, no_repository_here()).is_ok());
    }

    #[test]
    fn outside_a_repository_without_projects_hints_at_a_session_outside_one() {
        let err = outside_repository(&global_config(None), no_repository_here()).unwrap_err();
        let message = format!("{err:#}");
        assert!(
            message.contains("not inside a git repository"),
            "got: {message}"
        );
        assert!(
            message.contains("to start a session outside a repository"),
            "got: {message}"
        );
        // Git's own answer survives as the cause, so whichever failure it was
        // is still in the printed chain.
        assert!(
            message.contains("not a git repository (or any of the parent"),
            "got: {message}"
        );
    }

    /// No process runs at construction, so a real adapter stands in for the
    /// fakes other tests use.
    fn resolved_without_repo() -> Resolved {
        Resolved {
            cwd: PathBuf::from("/h"),
            gh: GhCli::new(SystemExec, PathBuf::from("/h")),
            repo: None,
            config: global_config(None),
            config_home: PathBuf::from("/h/.config"),
            home: PathBuf::from("/h"),
        }
    }

    #[test]
    fn a_command_that_needs_a_repository_says_so_outside_one() {
        let err = resolved_without_repo()
            .require_repo()
            .err()
            .unwrap()
            .to_string();
        assert_eq!(err, "this needs a git repository; run it inside one");
    }

    #[test]
    fn a_repo_less_provider_anchors_at_the_global_config_directory() {
        let resolved = Resolved {
            config_home: PathBuf::from("/c"),
            ..resolved_without_repo()
        };
        assert_eq!(resolved.provider_anchor(), PathBuf::from("/c/zax"));
    }
}
