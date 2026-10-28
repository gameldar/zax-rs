//! `WorktreeOpener` and `Editor` through the `herdr` CLI.
//!
//! zax's panes can run beside herdr rather than inside it, in a terminal
//! split of their own, so nothing here relies on `HERDR_PANE_ID` or any other
//! variable herdr gives its own panes: the CLI finds the running server
//! through its socket, and that is all these adapters need.

mod dto;

use std::path::Path;

use serde::de::DeserializeOwned;
use zax_app::{Editor, PortError, WorktreeOpener};
use zax_domain::Command;
use zax_exec::{shell_quote, Exec, ExecError, Output};

use crate::dto::{Failure, Reply, WorkspaceCreated, WorkspaceList, WorktreeList, WorktreeOpened};

const TOOL: &str = "herdr";
const INSTALL_HINT: &str = "install herdr from https://herdr.dev, or set HERDR_BIN_PATH \
                            to the herdr binary";
/// The skill a review agent is started with, followed by the pull request URL.
const REVIEW_SKILL: &str = "/pr-review";

/// The `herdr` CLI, which opens a worktree as a herdr space.
pub struct HerdrCli<E: Exec> {
    exec: E,
    /// `herdr`, or the binary `HERDR_BIN_PATH` names.
    program: String,
}

impl<E: Exec> HerdrCli<E> {
    pub fn new(exec: E, program: String) -> Self {
        Self { exec, program }
    }

    /// For tests that assert on the recorded calls.
    pub fn exec_ref(&self) -> &E {
        &self.exec
    }

    fn run(&self, args: &[&str]) -> Result<Output, PortError> {
        let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        let out = self
            .exec
            .exec(&self.program, &args, None, &[])
            .map_err(|e| match e {
                ExecError::NotFound { .. } => PortError::MissingProgram {
                    program: self.program.clone(),
                    hint: INSTALL_HINT.to_owned(),
                },
                ExecError::Io { detail, .. } => PortError::Incompatible {
                    tool: TOOL,
                    detail: format!("could not run `{}`: {detail}", self.program),
                },
            })?;
        if out.ok() {
            return Ok(out);
        }
        Err(PortError::CommandFailed {
            tool: TOOL,
            status: out.status,
            stderr: failure_message(&out),
        })
    }

    /// Run a command and read the `result` of its JSON reply.
    fn call<T: DeserializeOwned>(&self, args: &[&str]) -> Result<T, PortError> {
        let out = self.run(args)?;
        serde_json::from_str::<Reply<T>>(&out.stdout)
            .map(|reply| reply.result)
            .map_err(|e| PortError::Unparseable {
                tool: TOOL,
                detail: format!("`herdr {}`: {e}", args.join(" ")),
            })
    }

    fn focus(&self, workspace_id: &str) -> Result<(), PortError> {
        self.run(&["workspace", "focus", workspace_id]).map(|_| ())
    }

    fn probe_server(&self) -> Result<String, PortError> {
        let out = self.run(&["status", "server"])?;
        let field = |name: &str| {
            out.stdout
                .lines()
                .find_map(|line| line.strip_prefix(name)?.strip_prefix(':'))
                .map(str::trim)
        };
        match (field("status"), field("version")) {
            (Some("running"), version) => Ok(format!(
                "herdr {}, server running",
                version.unwrap_or("(unknown version)")
            )),
            _ => Err(PortError::Incompatible {
                tool: TOOL,
                detail: "the herdr server is not running; start herdr first".to_owned(),
            }),
        }
    }
}

/// herdr writes a failure as JSON on stderr. Its message is what a reader can
/// act on; the JSON around it is noise. Anything else is passed on as it is.
fn failure_message(out: &Output) -> String {
    [&out.stderr, &out.stdout]
        .into_iter()
        .find_map(|text| serde_json::from_str::<Failure>(text).ok())
        .map(|f| format!("{} ({})", f.error.message, f.error.code))
        .unwrap_or_else(|| out.stderr.trim().to_owned())
}

/// Whether herdr's listed path is this worktree. herdr may report the
/// resolved path where zax was handed one through a symlink, such as macOS's
/// `/tmp`, so a mismatch is checked again with both resolved.
fn same_path(listed: &str, path: &Path) -> bool {
    let listed = Path::new(listed);
    if listed == path {
        return true;
    }
    match (std::fs::canonicalize(listed), std::fs::canonicalize(path)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

impl<E: Exec> HerdrCli<E> {
    /// Focus the space that already has the worktree open, or open one. herdr
    /// opens a worktree only from the repository's own checkout, never from
    /// another linked worktree, so that is the directory it is opened from.
    ///
    /// Returns the new space's pane, or `None` when an existing space was
    /// focused: that space's pane may be busy, so nothing is run in it.
    fn open_or_focus(&self, path: &Path) -> Result<Option<String>, PortError> {
        let text = path.to_string_lossy();
        let list: WorktreeList = self.call(&["worktree", "list", "--cwd", &text])?;
        let open = list
            .worktrees
            .iter()
            .find(|w| same_path(&w.path, path))
            .and_then(|w| w.open_workspace_id.as_deref());
        if let Some(id) = open {
            return self.focus(id).map(|()| None);
        }
        let opened: WorktreeOpened = self.call(&[
            "worktree",
            "open",
            "--cwd",
            &list.source.source_checkout_path,
            "--path",
            &text,
            "--focus",
        ])?;
        Ok(Some(opened.root_pane.pane_id))
    }
}

/// herdr takes an agent name of at most 32 characters, a lowercase letter
/// then `[a-z0-9_-]`, so the pull request number is all of the URL it gets.
fn review_agent_name(pr_url: &str) -> String {
    let number = pr_url
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|n| !n.is_empty() && n.len() <= 20 && n.bytes().all(|b| b.is_ascii_digit()));
    match number {
        Some(n) => format!("review-{n}"),
        None => "review".to_owned(),
    }
}

impl<E: Exec> WorktreeOpener for HerdrCli<E> {
    fn open_worktree(&self, path: &Path) -> Result<(), PortError> {
        self.open_or_focus(path).map(|_| ())
    }

    /// Claude, started in the new space with the review skill on the pull
    /// request. A space that was already open is only focused.
    fn open_for_review(&self, path: &Path, pr_url: &str) -> Result<(), PortError> {
        let Some(pane) = self.open_or_focus(path)? else {
            return Ok(());
        };
        let prompt = format!("{REVIEW_SKILL} {pr_url}");
        self.run(&[
            "agent",
            "start",
            &review_agent_name(pr_url),
            "--kind",
            "claude",
            "--pane",
            &pane,
            "--",
            &prompt,
        ])
        .map(|_| ())
    }

    fn probe(&self) -> Result<String, PortError> {
        self.probe_server()
    }
}

/// A terminal editor run on a file in a herdr space of its own.
///
/// The space is labelled with the file's name, which is how a second edit of
/// the same file finds the space the first one opened. The editor replaces
/// the space's shell, so quitting it ends the space's only process and herdr
/// closes the space.
pub struct HerdrEditor<E: Exec> {
    herdr: HerdrCli<E>,
    command: Command,
}

impl<E: Exec> HerdrEditor<E> {
    pub fn new(herdr: HerdrCli<E>, command: Command) -> Self {
        Self { herdr, command }
    }
}

impl<E: Exec> Editor for HerdrEditor<E> {
    fn edit(&self, file: &Path) -> Result<(), PortError> {
        let label = file
            .file_name()
            .map_or_else(|| file.to_string_lossy(), |n| n.to_string_lossy())
            .into_owned();
        let spaces: WorkspaceList = self.herdr.call(&["workspace", "list"])?;
        if let Some(space) = spaces.workspaces.iter().find(|w| w.label == label) {
            return self.herdr.focus(&space.workspace_id);
        }

        let dir = file
            .parent()
            .filter(|d| !d.as_os_str().is_empty())
            .unwrap_or(Path::new("/"))
            .to_string_lossy()
            .into_owned();
        let created: WorkspaceCreated = self.herdr.call(&[
            "workspace",
            "create",
            "--cwd",
            &dir,
            "--label",
            &label,
            "--focus",
        ])?;

        let mut argv = self.command.argv().to_vec();
        argv.push(file.to_string_lossy().into_owned());
        let line = format!("exec {}", shell_quote(&argv));
        self.herdr
            .run(&["pane", "run", &created.root_pane.pane_id, &line])
            .map(|_| ())
    }

    /// The server, and then the editor itself: a space whose shell cannot
    /// find the editor is worse than no space, because the next edit focuses it.
    fn probe(&self) -> Result<String, PortError> {
        let server = self.herdr.probe_server()?;
        let program = self.command.program();
        let out = self
            .herdr
            .exec
            .exec(program, &["--version".to_owned()], None, &[])
            .map_err(|e| match e {
                ExecError::NotFound { .. } => PortError::MissingProgram {
                    program: program.to_owned(),
                    hint: format!(
                        "`{program}` comes from `editor.command`; `zax config` is where to change it"
                    ),
                },
                ExecError::Io { detail, .. } => PortError::Incompatible {
                    tool: "editor",
                    detail: format!("could not run `{program}`: {detail}"),
                },
            })?;
        let version = out.stdout.lines().next().unwrap_or_default();
        Ok(format!("{server}; {version}"))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use std::path::Path;

    use zax_app::{Editor, PortError, WorktreeOpener};
    use zax_domain::Command;
    use zax_exec::{EnvVar, Exec, ExecError, Output, RecordingExec};

    use super::*;

    fn ok(stdout: &str) -> Output {
        Output {
            status: 0,
            stdout: stdout.to_owned(),
            stderr: String::new(),
        }
    }

    fn herdr(responses: Vec<Output>) -> HerdrCli<RecordingExec> {
        HerdrCli::new(RecordingExec::with_responses(responses), "herdr".to_owned())
    }

    fn worktree_list(path: &str, open: Option<&str>) -> String {
        let open = open.map_or("null".to_owned(), |id| format!("\"{id}\""));
        format!(
            r#"{{"id":"cli:worktree:list","result":{{"type":"worktree_list",
            "source":{{"repo_root":"/src/acme","source_checkout_path":"/src/acme"}},
            "worktrees":[
              {{"path":"/src/acme","open_workspace_id":"w1","branch":"main"}},
              {{"path":"{path}","open_workspace_id":{open},"branch":"OMEGA-1"}}]}}}}"#
        )
    }

    const FOCUSED: &str = r#"{"id":"cli:workspace:focus","result":{"type":"workspace_info"}}"#;

    const OPENED: &str = r#"{"id":"cli:worktree:open","result":{"type":"worktree_opened",
        "already_open":false,"root_pane":{"pane_id":"w9:p1"},"workspace":{"workspace_id":"w9"}}}"#;

    #[test]
    fn a_worktree_already_open_in_a_space_focuses_that_space() {
        let wt = "/src/worktrees/acme/OMEGA-1-x";
        let herdr = herdr(vec![ok(&worktree_list(wt, Some("w9"))), ok(FOCUSED)]);

        herdr.open_worktree(Path::new(wt)).unwrap();

        let exec = herdr.exec_ref();
        assert_eq!(exec.args(0), ["worktree", "list", "--cwd", wt]);
        assert_eq!(exec.args(1), ["workspace", "focus", "w9"]);
        assert_eq!(exec.calls().len(), 2);
    }

    #[test]
    fn a_worktree_no_space_has_open_is_opened_as_one() {
        let wt = "/src/worktrees/acme/OMEGA-1-x";
        let herdr = herdr(vec![ok(&worktree_list(wt, None)), ok(OPENED)]);

        herdr.open_worktree(Path::new(wt)).unwrap();

        // From the repository's own checkout: herdr refuses to open a
        // worktree starting from another linked worktree.
        assert_eq!(
            herdr.exec_ref().args(1),
            [
                "worktree",
                "open",
                "--cwd",
                "/src/acme",
                "--path",
                wt,
                "--focus"
            ]
        );
    }

    #[test]
    fn a_review_starts_claude_on_the_pull_request_in_the_new_space() {
        let wt = "/src/worktrees/acme/patch-1";
        let url = "https://github.com/acme/omega/pull/1963";
        let herdr = herdr(vec![
            ok(&worktree_list(wt, None)),
            ok(OPENED),
            ok(r#"{"id":"cli:agent:start","result":{"type":"agent_started"}}"#),
        ]);

        herdr.open_for_review(Path::new(wt), url).unwrap();

        assert_eq!(
            herdr.exec_ref().args(2),
            [
                "agent",
                "start",
                "review-1963",
                "--kind",
                "claude",
                "--pane",
                "w9:p1",
                "--",
                "/pr-review https://github.com/acme/omega/pull/1963"
            ]
        );
    }

    #[test]
    fn a_review_of_a_worktree_already_open_only_focuses_its_space() {
        // Its pane may be running anything, including a review already.
        let wt = "/src/worktrees/acme/patch-1";
        let herdr = herdr(vec![ok(&worktree_list(wt, Some("w9"))), ok(FOCUSED)]);

        herdr
            .open_for_review(Path::new(wt), "https://github.com/acme/omega/pull/1963")
            .unwrap();

        let exec = herdr.exec_ref();
        assert_eq!(exec.args(1), ["workspace", "focus", "w9"]);
        assert_eq!(exec.calls().len(), 2);
    }

    #[test]
    fn a_review_agent_is_named_for_the_pull_request_number() {
        assert_eq!(
            review_agent_name("https://github.com/acme/omega/pull/1963"),
            "review-1963"
        );
        assert_eq!(
            review_agent_name("https://github.com/acme/omega/pull/1963/"),
            "review-1963"
        );
        assert_eq!(review_agent_name("https://example.com/odd"), "review");
    }

    #[test]
    fn a_worktree_named_through_a_symlink_still_finds_its_space() {
        // macOS's /tmp is /private/tmp, and herdr reports the resolved path.
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let resolved = std::fs::canonicalize(&real).unwrap();
        let listed = worktree_list(&resolved.display().to_string(), Some("w9"));
        let herdr = herdr(vec![ok(&listed), ok(FOCUSED)]);

        herdr.open_worktree(&link).unwrap();

        assert_eq!(herdr.exec_ref().args(1), ["workspace", "focus", "w9"]);
    }

    #[test]
    fn a_herdr_failure_says_herdrs_own_message_not_its_json() {
        let herdr = herdr(vec![Output {
            status: 1,
            stdout: String::new(),
            stderr: r#"{"error":{"code":"not_git_worktree","message":"Herdr worktree actions require a path inside a Git work tree"},"id":"cli:worktree:list"}"#.to_owned(),
        }]);

        let message = herdr
            .open_worktree(Path::new("/tmp"))
            .unwrap_err()
            .to_string();

        assert!(
            message.contains("require a path inside a Git work tree"),
            "got: {message}"
        );
        assert!(!message.contains('{'), "got: {message}");
    }

    #[test]
    fn a_missing_herdr_says_what_to_install() {
        struct MissingExec;
        impl Exec for MissingExec {
            fn exec(
                &self,
                program: &str,
                _: &[String],
                _: Option<&Path>,
                _: &[EnvVar],
            ) -> Result<Output, ExecError> {
                Err(ExecError::NotFound {
                    program: program.to_owned(),
                })
            }
        }
        let herdr = HerdrCli::new(MissingExec, "herdr".to_owned());

        let err = herdr.open_worktree(Path::new("/src/acme")).unwrap_err();

        assert!(matches!(err, PortError::MissingProgram { .. }), "{err:?}");
        assert!(err.to_string().contains("herdr.dev"), "{err}");
    }

    #[test]
    fn a_probe_reports_the_running_servers_version() {
        let herdr = herdr(vec![ok(
            "status: running\nversion: 0.9.1\nendpoint_compatible: yes\n",
        )]);

        let detail = WorktreeOpener::probe(&herdr).unwrap();

        assert_eq!(herdr.exec_ref().args(0), ["status", "server"]);
        assert_eq!(detail, "herdr 0.9.1, server running");
    }

    #[test]
    fn a_probe_with_no_server_running_fails() {
        let herdr = herdr(vec![ok("status: stopped\n")]);

        let message = WorktreeOpener::probe(&herdr).unwrap_err().to_string();

        assert!(message.contains("not running"), "got: {message}");
    }

    fn editor(responses: Vec<Output>) -> HerdrEditor<RecordingExec> {
        HerdrEditor::new(
            herdr(responses),
            Command::new(vec!["nvim".to_owned()]).unwrap(),
        )
    }

    const SPACES: &str = r#"{"id":"cli:workspace:list","result":{"type":"workspace_list","workspaces":[
        {"workspace_id":"w2","label":"acme","focused":true},
        {"workspace_id":"w7","label":"todo.md","focused":false}]}}"#;

    const NO_TODO_SPACE: &str = r#"{"id":"cli:workspace:list","result":{"type":"workspace_list","workspaces":[
        {"workspace_id":"w2","label":"acme","focused":true}]}}"#;

    const CREATED: &str = r#"{"id":"cli:workspace:create","result":{"type":"workspace_created",
        "workspace":{"workspace_id":"w8"},"tab":{"tab_id":"w8:t1"},"root_pane":{"pane_id":"w8:p1"}}}"#;

    #[test]
    fn a_file_already_open_in_its_space_focuses_that_space() {
        let editor = editor(vec![ok(SPACES), ok(FOCUSED)]);

        editor
            .edit(Path::new("/home/k/.local/share/zax/todo.md"))
            .unwrap();

        let exec = editor.herdr.exec_ref();
        assert_eq!(exec.args(0), ["workspace", "list"]);
        assert_eq!(exec.args(1), ["workspace", "focus", "w7"]);
        assert_eq!(exec.calls().len(), 2);
    }

    #[test]
    fn a_file_with_no_space_gets_one_whose_shell_is_replaced_by_the_editor() {
        // `exec`, so quitting the editor ends the space's only process and
        // herdr closes the space.
        let editor = editor(vec![ok(NO_TODO_SPACE), ok(CREATED), ok("{}")]);

        editor
            .edit(Path::new("/home/k/.local/share/zax/todo.md"))
            .unwrap();

        let exec = editor.herdr.exec_ref();
        assert_eq!(
            exec.args(1),
            [
                "workspace",
                "create",
                "--cwd",
                "/home/k/.local/share/zax",
                "--label",
                "todo.md",
                "--focus"
            ]
        );
        assert_eq!(
            exec.args(2),
            [
                "pane",
                "run",
                "w8:p1",
                "exec nvim /home/k/.local/share/zax/todo.md"
            ]
        );
    }

    #[test]
    fn a_file_whose_path_has_spaces_and_quotes_reaches_the_editor_whole() {
        let editor = editor(vec![ok(NO_TODO_SPACE), ok(CREATED), ok("{}")]);

        editor.edit(Path::new("/home/k/My Notes/it's.md")).unwrap();

        assert_eq!(
            editor.herdr.exec_ref().args(2)[3],
            r"exec nvim '/home/k/My Notes/it'\''s.md'"
        );
    }

    #[test]
    fn an_editor_probe_reports_the_server_and_the_editor() {
        let editor = editor(vec![
            ok("status: running\nversion: 0.9.1\n"),
            ok("NVIM v0.10.4\nBuild type: Release\n"),
        ]);

        let detail = Editor::probe(&editor).unwrap();

        let calls = editor.herdr.exec_ref().calls();
        assert_eq!(calls[1].program, "nvim");
        assert_eq!(calls[1].args, ["--version"]);
        assert_eq!(detail, "herdr 0.9.1, server running; NVIM v0.10.4");
    }

    #[test]
    fn an_editor_probe_fails_when_the_editor_is_not_installed() {
        // Otherwise doctor says ok and the first `SPC o f` opens a space whose shell
        // cannot find the editor, which the next `SPC o f` would then focus.
        struct NoEditor;
        impl Exec for NoEditor {
            fn exec(
                &self,
                program: &str,
                _: &[String],
                _: Option<&Path>,
                _: &[EnvVar],
            ) -> Result<Output, ExecError> {
                if program == "herdr" {
                    return Ok(ok("status: running\nversion: 0.9.1\n"));
                }
                Err(ExecError::NotFound {
                    program: program.to_owned(),
                })
            }
        }
        let editor = HerdrEditor::new(
            HerdrCli::new(NoEditor, "herdr".to_owned()),
            Command::new(vec!["nvim".to_owned()]).unwrap(),
        );

        let err = Editor::probe(&editor).unwrap_err();

        assert!(matches!(err, PortError::MissingProgram { .. }), "{err:?}");
        assert!(err.to_string().contains("editor.command"), "{err}");
    }

    #[test]
    fn a_herdr_reply_zax_cannot_read_is_unparseable_not_a_panic() {
        let editor = editor(vec![ok("not json")]);

        let err = editor.edit(Path::new("/home/k/todo.md")).unwrap_err();

        assert!(matches!(err, PortError::Unparseable { .. }), "{err:?}");
    }
}
