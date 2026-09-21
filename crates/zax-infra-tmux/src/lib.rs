//! `Multiplexer` via tmux, always on a dedicated socket with our own config.

pub mod argv;
pub mod conf;
pub mod diagnostics;
pub mod quote;

use std::path::{Path, PathBuf};

use crate::diagnostics::{hint_for, resolve_on_path, Diagnostics};
use zax_app::{Multiplexer, PortError};
use zax_domain::{Command, PaneSize, SessionName};
use zax_exec::{Exec, ExecError, Handover, Output};

const TOOL: &str = "tmux";

/// The socket zax uses. A separate socket means zax cannot inherit or disturb a
/// personal tmux setup, and prefix-key choices are ours to make.
pub const SOCKET_DEFAULT: &str = "zax";

pub struct TmuxCli<E: Exec, H: Handover> {
    exec: E,
    handover: H,
    socket: String,
    conf: PathBuf,
    program: String,
}

impl<E: Exec, H: Handover> TmuxCli<E, H> {
    pub fn new(exec: E, handover: H, socket: String, conf: PathBuf) -> Self {
        Self {
            exec,
            handover,
            socket,
            conf,
            program: TOOL.to_owned(),
        }
    }

    pub fn with_program(mut self, program: String) -> Self {
        self.program = program;
        self
    }

    pub fn exec_ref(&self) -> &E {
        &self.exec
    }

    pub fn handover_ref(&self) -> &H {
        &self.handover
    }

    fn base(&self) -> argv::Base<'_> {
        argv::Base {
            socket: &self.socket,
            // `Base` holds a `&str`, so the conf path is kept as one here.
            conf: self.conf_str(),
        }
    }

    fn conf_str(&self) -> &str {
        // A non-UTF-8 config path is not supportable; falling back to an empty
        // string makes tmux report the problem rather than zax panicking.
        self.conf.to_str().unwrap_or("")
    }

    fn run(&self, args: Vec<String>) -> Result<Output, PortError> {
        self.exec
            .exec(&self.program, &args, None, &[])
            .map_err(map_exec_error)
    }

    fn run_ok(&self, operation: &'static str, args: Vec<String>) -> Result<Output, PortError> {
        let out = self.run(args.clone())?;
        if out.ok() {
            Ok(out)
        } else {
            Err(self.failure(operation, &args, &out))
        }
    }

    /// Turn a non-zero tmux exit into an error that names its own context.
    /// tmux is driven through a socket that outlives any one zax invocation,
    /// so the environment is as likely to be the cause as the command is.
    fn failure(&self, operation: &'static str, argv: &[String], out: &Output) -> PortError {
        let stderr = out.stderr.trim().to_owned();
        let diagnostics = Diagnostics {
            argv: argv.to_vec(),
            socket: self.socket.clone(),
            conf: self.conf.clone(),
            binary: self.resolved_binary(),
            // Asking a tmux that just failed for its version can fail too, and
            // a failure while reporting a failure helps nobody. The version is
            // read from the binary, not the server, so it is safe to ask.
            version: self.version_of_binary(),
            hint: hint_for(&stderr, &self.socket),
        };
        PortError::CommandFailedInContext {
            tool: TOOL,
            operation,
            status: out.status,
            stderr,
            context: diagnostics.render(),
        }
    }

    /// Where the tmux zax runs actually lives. Resolved rather than assumed,
    /// because "which tmux" is the question a version mismatch turns on.
    fn resolved_binary(&self) -> Option<PathBuf> {
        let path_var = std::env::var("PATH").ok()?;
        resolve_on_path(&self.program, &path_var, &is_executable)
    }

    /// How the socket answers right now. A tmux that runs but cannot reach
    /// the server already on its socket is not a working tmux for zax's
    /// purposes, so that case is an error rather than a footnote on a line
    /// that otherwise reads as success.
    ///
    /// Having no server yet is not a fault: it is the ordinary state before
    /// the first session of the day.
    fn socket_health(&self) -> Result<&'static str, PortError> {
        let args = argv::list_sessions(self.base());
        let out = self.run(args.clone())?;
        if out.ok() {
            return Ok("reachable");
        }
        let stderr = out.stderr.trim().to_ascii_lowercase();
        if stderr.contains("no server running") || stderr.contains("no such file or directory") {
            return Ok("no server yet, which is normal before the first session");
        }
        Err(self.failure("list-sessions", &args, &out))
    }

    /// The version of the binary on PATH, which is the client's version. It
    /// need not match the server already running on the socket, and that
    /// mismatch is exactly what this is here to expose.
    fn version_of_binary(&self) -> Option<String> {
        let out = self
            .exec
            .exec(&self.program, &["-V".to_owned()], None, &[])
            .ok()?;
        out.ok().then(|| out.trimmed_stdout().to_owned())
    }
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

fn map_exec_error(error: ExecError) -> PortError {
    match error {
        ExecError::NotFound { .. } => PortError::MissingTool { tool: TOOL },
        ExecError::Io { detail, .. } => PortError::Unparseable { tool: TOOL, detail },
    }
}

impl<E: Exec, H: Handover> Multiplexer for TmuxCli<E, H> {
    fn session_exists(&self, name: &SessionName) -> Result<bool, PortError> {
        let args = argv::has_session(self.base(), name);
        let out = self.run(args.clone())?;
        if out.ok() {
            return Ok(true);
        }
        // tmux answers "not there" with a non-zero exit and a message, and it
        // has no distinct exit code for it, so the message is all there is to
        // go on. The matching is therefore best effort, and deliberately
        // narrow: anything unrecognised becomes an error rather than a "no".
        // A tmux that failed for some other reason must not read as an empty
        // name for the caller to build over.
        let stderr = out.stderr.to_ascii_lowercase();
        let absent = stderr.contains("can't find session")
            // No socket file at all: nothing has ever started this server.
            || stderr.contains("no such file or directory")
            // A socket file whose server has gone.
            || stderr.contains("no server running");
        if absent {
            return Ok(false);
        }
        Err(self.failure("has-session", &args, &out))
    }

    fn list_sessions(&self) -> Result<Vec<SessionName>, PortError> {
        let args = argv::list_sessions(self.base());
        let out = self.run(args.clone())?;
        if !out.ok() {
            // "no server running" means no sessions, not a failure.
            if out.stderr.contains("no server running") {
                return Ok(Vec::new());
            }
            return Err(self.failure("list-sessions", &args, &out));
        }
        // Someone else's session on this socket is not ours to report.
        Ok(out
            .stdout
            .lines()
            .filter_map(|l| SessionName::parse(l.trim()).ok())
            .collect())
    }

    fn create_session(
        &self,
        name: &SessionName,
        cwd: &Path,
        command: &Command,
    ) -> Result<(), PortError> {
        self.run_ok(
            "new-session",
            argv::new_session(self.base(), name, cwd, command),
        )
        .map(|_| ())
    }

    fn split_pane(
        &self,
        name: &SessionName,
        target_pane: u32,
        horizontal: bool,
        size: Option<PaneSize>,
        cwd: &Path,
        command: &Command,
    ) -> Result<(), PortError> {
        self.run_ok(
            "split-window",
            argv::split_window(
                self.base(),
                name,
                target_pane,
                horizontal,
                size,
                cwd,
                command,
            ),
        )
        .map(|_| ())
    }

    fn select_pane(&self, name: &SessionName, index: u32) -> Result<(), PortError> {
        self.run_ok("select-pane", argv::select_pane(self.base(), name, index))
            .map(|_| ())
    }

    fn attach(&self, name: &SessionName) -> Result<(), PortError> {
        let args = argv::attach_session(self.base(), name);
        self.handover
            .handover(&self.program, &args)
            .map_err(map_exec_error)
    }

    fn kill_session(&self, name: &SessionName) -> Result<(), PortError> {
        self.run_ok("kill-session", argv::kill_session(self.base(), name))
            .map(|_| ())
    }

    /// `zax doctor`'s tmux line. It reports the client binary AND whether the
    /// socket can be reached, because a perfectly good tmux that cannot talk
    /// to the server already on the socket is the failure this answers.
    fn probe(&self) -> Result<String, PortError> {
        let version = self
            .run_ok("-V", argv::version(self.base()))?
            .trimmed_stdout()
            .to_owned();
        let where_found = match self.resolved_binary() {
            Some(path) => format!(" at {}", path.display()),
            None => String::new(),
        };
        Ok(format!(
            "{version}{where_found}; socket \"{}\": {}",
            self.socket,
            self.socket_health()?
        ))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use std::path::{Path, PathBuf};

    use zax_app::{Multiplexer, PortError};
    use zax_domain::{BranchName, Command, RootRepoName, SessionName};
    use zax_exec::{Output, RecordingExec, RecordingHandover};

    use super::*;

    fn name() -> SessionName {
        SessionName::derive(
            &RootRepoName::parse("acme").unwrap(),
            &BranchName::parse_local("OMEGA-1963-stamp").unwrap(),
        )
    }

    fn cli(responses: Vec<Output>) -> TmuxCli<RecordingExec, RecordingHandover> {
        TmuxCli::new(
            RecordingExec::with_responses(responses),
            RecordingHandover::default(),
            SOCKET_DEFAULT.to_owned(),
            PathBuf::from("/run/zax/tmux.conf"),
        )
    }

    fn ok(stdout: &str) -> Output {
        Output {
            status: 0,
            stdout: stdout.to_owned(),
            stderr: String::new(),
        }
    }

    fn failed(status: i32, stderr: &str) -> Output {
        Output {
            status,
            stdout: String::new(),
            stderr: stderr.to_owned(),
        }
    }

    #[test]
    fn session_existence_reads_the_exit_status_rather_than_erroring() {
        assert!(cli(vec![ok("")]).session_exists(&name()).unwrap());
        assert!(!cli(vec![failed(1, "can't find session")])
            .session_exists(&name())
            .unwrap());
    }

    #[test]
    fn the_spellings_of_a_genuinely_absent_session_are_all_a_plain_no() {
        // Exactly what tmux 3.7 writes, in the three states that mean "not
        // there": no socket file, a dead server behind one, and a live server
        // without that session.
        for stderr in [
            "error connecting to /private/tmp/tmux-501/zax (No such file or directory)",
            "no server running on /private/tmp/tmux-501/zax",
            "can't find session: zax-acme-topic-1234abcd",
        ] {
            assert!(
                !cli(vec![failed(1, stderr)])
                    .session_exists(&name())
                    .unwrap(),
                "{stderr} means the session is absent"
            );
        }
    }

    #[test]
    fn a_tmux_that_failed_for_another_reason_is_not_an_empty_slot() {
        // Reading every non-zero exit as "no session" turns a broken tmux into
        // a free name, and the caller then builds over whatever is there.
        let err = cli(vec![failed(1, "lost server")])
            .session_exists(&name())
            .unwrap_err();
        assert!(
            matches!(
                err,
                PortError::CommandFailedInContext {
                    tool: "tmux",
                    operation: "has-session",
                    ..
                }
            ),
            "got: {err:?}"
        );
        assert!(err.to_string().contains("lost server"), "got: {err}");
    }

    #[test]
    fn listing_sessions_with_no_server_running_is_an_empty_list() {
        // A cold machine has no zax server yet, which is not an error.
        let tmux = cli(vec![failed(1, "no server running on /tmp/tmux-501/zax")]);
        assert_eq!(tmux.list_sessions().unwrap(), vec![]);
    }

    #[test]
    fn listing_sessions_keeps_only_names_that_are_zax_sessions() {
        let tmux = cli(vec![ok(
            "zax-acme-topic\npersonal\nzax-omega-OMEGA-1-a\n",
        )]);
        let names: Vec<_> = tmux
            .list_sessions()
            .unwrap()
            .into_iter()
            .map(|n| n.as_str().to_owned())
            .collect();
        assert_eq!(names, ["zax-acme-topic", "zax-omega-OMEGA-1-a"]);
    }

    #[test]
    fn a_real_tmux_failure_is_still_an_error() {
        let tmux = cli(vec![failed(1, "unknown command: nonsense")]);
        let err = tmux
            .create_session(
                &name(),
                Path::new("/w"),
                &Command::new(vec!["claude".to_owned()]).unwrap(),
            )
            .unwrap_err();
        assert!(
            matches!(
                err,
                PortError::CommandFailedInContext {
                    tool: "tmux",
                    operation: "new-session",
                    ..
                }
            ),
            "got: {err:?}"
        );
    }

    #[test]
    fn a_failure_names_the_operation_socket_and_config_not_just_the_stderr() {
        // The bare stderr is what sent a real user hunting; the context block
        // is the fix. Assert on the rendered message a person actually reads.
        let tmux = cli(vec![failed(1, "server exited unexpectedly")]);
        let err = tmux.kill_session(&name()).unwrap_err().to_string();
        assert!(err.contains("tmux kill-session failed"), "got:\n{err}");
        assert!(err.contains("server exited unexpectedly"), "got:\n{err}");
        assert!(err.contains("while running: tmux -L zax"), "got:\n{err}");
        assert!(err.contains("socket:        zax"), "got:\n{err}");
        assert!(err.contains("/run/zax/tmux.conf"), "got:\n{err}");
    }

    #[test]
    fn a_version_mismatch_is_explained_rather_than_merely_reported() {
        // "server exited unexpectedly" is what tmux says when a client of one
        // build meets a server of another. Nothing in those three words tells
        // the user that, so zax has to.
        let tmux = cli(vec![failed(1, "server exited unexpectedly")]);
        let err = tmux.kill_session(&name()).unwrap_err().to_string();
        assert!(err.contains("different tmux build"), "got:\n{err}");
        assert!(err.contains("pgrep"), "got:\n{err}");
    }

    #[test]
    fn an_ordinary_failure_is_not_dressed_up_with_a_mismatch_hint() {
        let tmux = cli(vec![failed(1, "duplicate session: zax-a-b")]);
        let err = tmux.kill_session(&name()).unwrap_err().to_string();
        assert!(!err.contains("different tmux build"), "got:\n{err}");
        assert!(err.contains("duplicate session"), "got:\n{err}");
    }

    #[test]
    fn creating_a_session_builds_the_expected_command_line() {
        let tmux = cli(vec![ok("")]);
        tmux.create_session(
            &name(),
            Path::new("/w"),
            &Command::new(vec!["claude".to_owned()]).unwrap(),
        )
        .unwrap();
        assert_eq!(
            tmux.exec_ref().args(0),
            [
                "-L".to_owned(),
                "zax".to_owned(),
                "-f".to_owned(),
                "/run/zax/tmux.conf".to_owned(),
                "new-session".to_owned(),
                "-d".to_owned(),
                "-s".to_owned(),
                name().as_str().to_owned(),
                "-c".to_owned(),
                "/w".to_owned(),
                "claude".to_owned(),
            ]
        );
    }

    #[test]
    fn attaching_hands_the_terminal_over_rather_than_capturing_output() {
        let tmux = cli(vec![]);
        tmux.attach(&name()).unwrap();
        assert!(
            tmux.exec_ref().calls().is_empty(),
            "attach must not be captured"
        );
        let handed = tmux.handover_ref().calls();
        assert_eq!(handed.len(), 1);
        assert_eq!(handed[0].0, "tmux");
        assert_eq!(
            handed[0].1,
            [
                "-L".to_owned(),
                "zax".to_owned(),
                "-f".to_owned(),
                "/run/zax/tmux.conf".to_owned(),
                "attach-session".to_owned(),
                "-t".to_owned(),
                format!("={}", name().as_str()),
            ]
        );
    }

    #[test]
    fn a_missing_tmux_binary_is_reported_as_a_missing_tool() {
        let tmux = TmuxCli::new(
            zax_exec::SystemExec,
            zax_exec::SystemHandover,
            "zax-nonexistent".to_owned(),
            PathBuf::from("/dev/null"),
        )
        .with_program("definitely-not-tmux-zax".to_owned());
        assert!(matches!(
            tmux.probe().unwrap_err(),
            PortError::MissingTool { tool: "tmux" }
        ));
    }
}
