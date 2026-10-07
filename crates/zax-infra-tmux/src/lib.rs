//! `Multiplexer` via tmux, always on a dedicated socket with our own config.

pub mod argv;
pub mod conf;
pub mod diagnostics;
pub mod quote;

use std::path::{Path, PathBuf};

use crate::diagnostics::{hint_for, resolve_on_path, Diagnostics};
use zax_app::{Multiplexer, PortError};
use zax_domain::{Command, LiveSession, PaneSize, SessionName};
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
    fn socket_health(&self) -> Result<SocketHealth, PortError> {
        let args = argv::list_sessions(self.base());
        let out = self.run(args.clone())?;
        if out.ok() {
            return Ok(SocketHealth::Reachable);
        }
        if no_server(&out.stderr) {
            return Ok(SocketHealth::NoServer);
        }
        Err(self.failure("list-sessions", &args, &out))
    }

    /// Only a reachable socket has a server to ask.
    fn server_version_if(&self, health: SocketHealth) -> Option<String> {
        (health == SocketHealth::Reachable)
            .then(|| self.server_version())
            .flatten()
    }

    /// The version the server on the socket was built from, if there is a
    /// server and it answers.
    fn server_version(&self) -> Option<String> {
        let out = self.run(argv::server_version(self.base())).ok()?;
        out.ok().then(|| out.trimmed_stdout().to_owned())
    }

    /// A client of one tmux version and a server of another can still run
    /// one-shot commands against each other, which is why sessions still get
    /// built, but attaching fails with "open terminal failed: not a terminal".
    /// Because the server outlives every zax invocation, a `PATH` that differs
    /// between two shells - a direnv or nix shell in one repository - is
    /// enough to produce it.
    ///
    /// Best effort: `None` whenever either version cannot be read, because a
    /// check that could not run is no reason to refuse.
    fn version_mismatch(&self) -> Option<PortError> {
        let client = self.version_of_binary()?;
        let server = self.server_version()?;
        self.mismatch_between(&client, &server)
    }

    /// `client` as `tmux -V` prints it, `server` as `#{version}` does.
    fn mismatch_between(&self, client: &str, server: &str) -> Option<PortError> {
        let client = client.strip_prefix("tmux ").unwrap_or(client);
        if client == server {
            return None;
        }
        let at = self
            .resolved_binary()
            .map(|p| format!(" at {}", p.display()))
            .unwrap_or_default();
        Some(PortError::Incompatible {
            tool: TOOL,
            detail: format!(
                "this is tmux {client}{at}, but the zax server on socket \"{socket}\" \
is tmux {server}. Attaching across versions fails with \"open terminal failed: \
not a terminal\". Put tmux {server} first on PATH here, or restart the server \
with `tmux -L {socket} kill-server`, which ends every zax session.",
                socket = self.socket,
            ),
        })
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

/// What `list-sessions` says about the socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SocketHealth {
    Reachable,
    NoServer,
}

impl SocketHealth {
    fn describe(self) -> &'static str {
        match self {
            Self::Reachable => "reachable",
            Self::NoServer => "no server yet, which is normal before the first session",
        }
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

/// tmux's spellings of "there is no server on this socket": no socket file at
/// all, because nothing has ever started it, or a socket file whose server has
/// gone. Both are the ordinary state before the first session of the day.
fn no_server(stderr: &str) -> bool {
    let stderr = stderr.to_ascii_lowercase();
    stderr.contains("no such file or directory") || stderr.contains("no server running")
}

/// No server left to answer a question about sessions.
///
/// Wider than `no_server`, by the one spelling tmux uses for a server that was
/// there a moment ago: killing the last session makes the server exit, and a
/// query that arrives while it is shutting down has its connection dropped
/// rather than being told there is no server. The session the caller asked
/// about is gone in every one of those cases.
///
/// Deliberately not used by `socket_health`. tmux says the same thing when a
/// client meets a server of another build, and that server is still there
/// holding the socket - reading it as "no server yet" would be how `zax
/// doctor` stopped reporting the mismatch it exists to catch. A query can
/// answer "no sessions" and be right either way; the doctor cannot.
fn no_server_to_query(stderr: &str) -> bool {
    no_server(stderr)
        || stderr
            .to_ascii_lowercase()
            .contains("server exited unexpectedly")
}

/// One `list-sessions` line. Someone else's session on this socket is not ours
/// to report, so it is skipped rather than parsed.
fn parse_listed_session(line: &str) -> Option<Result<LiveSession, PortError>> {
    let line = line.trim();
    let (raw_name, clients) = line.rsplit_once(' ').unwrap_or((line, ""));
    let name = SessionName::parse(raw_name).ok()?;
    Some(
        clients
            .parse::<u32>()
            .map(|clients| LiveSession {
                name,
                attached: clients > 0,
            })
            .map_err(|_| PortError::Unparseable {
                tool: TOOL,
                detail: format!("list-sessions line without a client count: `{line}`"),
            }),
    )
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
        let absent = out
            .stderr
            .to_ascii_lowercase()
            .contains("can't find session")
            || no_server_to_query(&out.stderr);
        if absent {
            return Ok(false);
        }
        Err(self.failure("has-session", &args, &out))
    }

    fn list_sessions(&self) -> Result<Vec<LiveSession>, PortError> {
        let args = argv::list_sessions(self.base());
        let out = self.run(args.clone())?;
        if !out.ok() {
            // No server, or no socket file at all, means no sessions rather
            // than a failure.
            if no_server_to_query(&out.stderr) {
                return Ok(Vec::new());
            }
            return Err(self.failure("list-sessions", &args, &out));
        }
        out.stdout
            .lines()
            .filter_map(parse_listed_session)
            .collect()
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
        // Once the terminal is handed over, tmux's own error is all the user
        // sees, and across versions that error is "not a terminal". So the
        // mismatch is looked for first, while zax can still explain it.
        if let Some(mismatch) = self.version_mismatch() {
            return Err(mismatch);
        }
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
        let health = self.socket_health()?;
        let server = match self.server_version_if(health) {
            Some(server) => {
                if let Some(mismatch) = self.mismatch_between(&version, &server) {
                    return Err(mismatch);
                }
                format!(", server tmux {server}")
            }
            None => String::new(),
        };
        Ok(format!(
            "{version}{where_found}; socket \"{}\": {}{server}",
            self.socket,
            health.describe()
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
    fn listing_sessions_before_the_socket_exists_is_an_empty_list() {
        // `session start` lists before it creates, so the very first session
        // on a machine meets a socket file that is not there yet.
        let tmux = cli(vec![failed(
            1,
            "error connecting to /private/tmp/tmux-501/zax (No such file or directory)",
        )]);
        assert_eq!(tmux.list_sessions().unwrap(), vec![]);
    }

    #[test]
    fn listing_sessions_keeps_only_names_that_are_zax_sessions() {
        let tmux = cli(vec![ok(
            "zax-acme-topic 0\npersonal 1\nzax-omega-OMEGA-1-a 2\n",
        )]);
        let names: Vec<_> = tmux
            .list_sessions()
            .unwrap()
            .into_iter()
            .map(|s| s.name.as_str().to_owned())
            .collect();
        assert_eq!(names, ["zax-acme-topic", "zax-omega-OMEGA-1-a"]);
    }

    #[test]
    fn a_session_with_any_client_is_attached_and_one_with_none_is_not() {
        // `#{session_attached}` is a client count, not a flag.
        let tmux = cli(vec![ok("zax-a 0\nzax-b 1\nzax-c 2\n")]);
        let attached: Vec<_> = tmux
            .list_sessions()
            .unwrap()
            .into_iter()
            .map(|s| (s.name.as_str().to_owned(), s.attached))
            .collect();
        assert_eq!(
            attached,
            [
                ("zax-a".to_owned(), false),
                ("zax-b".to_owned(), true),
                ("zax-c".to_owned(), true)
            ]
        );
    }

    #[test]
    fn a_listing_line_zax_cannot_read_is_an_error_not_a_detached_session() {
        // Reading garbage as "nobody attached" would hand this agent to a
        // second Zed thread as if it were free.
        let err = cli(vec![ok("zax-a lots\n")]).list_sessions().unwrap_err();
        assert!(
            matches!(err, PortError::Unparseable { tool: "tmux", .. }),
            "got: {err:?}"
        );
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
    fn a_server_going_down_with_its_last_session_answers_no_rather_than_failing() {
        // Killing the only session makes the server exit, and a query that
        // lands while it is on the way out has its connection dropped. tmux
        // spells that the same way it spells a version mismatch, but the
        // answer to "is that session there" is no either way.
        let tmux = cli(vec![failed(1, "server exited unexpectedly")]);

        assert!(!tmux.session_exists(&name()).unwrap());
    }

    #[test]
    fn a_server_going_down_lists_no_sessions_rather_than_failing() {
        let tmux = cli(vec![failed(1, "server exited unexpectedly")]);

        assert!(tmux.list_sessions().unwrap().is_empty());
    }

    #[test]
    fn the_doctor_still_reports_a_server_it_cannot_talk_to() {
        // The reason a query's "no server" is wider than the doctor's: a
        // mismatched server is still holding the socket and says exactly what
        // a shutting-down one says. A query may answer "no sessions" and be
        // right; the doctor calling it "no server yet" would hide the fault.
        let tmux = cli(vec![
            ok("tmux 3.7c\n"),
            failed(1, "server exited unexpectedly"),
        ]);

        let err = tmux.probe().unwrap_err().to_string();

        assert!(err.contains("different tmux build"), "got:\n{err}");
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
                "env -u TMUX claude".to_owned(),
            ]
        );
    }

    #[test]
    fn attaching_hands_the_terminal_over_rather_than_capturing_output() {
        // No responses at all: the version check before attaching fails, and
        // a check that could not run is no reason to refuse the attach.
        let tmux = cli(vec![]);
        tmux.attach(&name()).unwrap();
        assert!(
            !tmux
                .exec_ref()
                .calls()
                .iter()
                .any(|c| c.args.contains(&"attach-session".to_owned())),
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
    fn attaching_across_tmux_versions_is_refused_with_an_explanation() {
        // A 3.7c client attaching to a 3.5a server gets "open terminal failed:
        // not a terminal", which says nothing about versions. After the
        // handover zax can no longer explain it, so it has to refuse first.
        let tmux = cli(vec![ok("tmux 3.7c\n"), ok("3.5a\n")]);
        let err = tmux.attach(&name()).unwrap_err();
        assert!(
            matches!(err, PortError::Incompatible { tool: "tmux", .. }),
            "got: {err:?}"
        );
        let err = err.to_string();
        assert!(err.contains("3.7c") && err.contains("3.5a"), "got: {err}");
        assert!(err.contains("not a terminal"), "got: {err}");
        assert!(err.contains("-L zax kill-server"), "got: {err}");
        assert!(
            tmux.handover_ref().calls().is_empty(),
            "a refused attach must not hand over"
        );
    }

    #[test]
    fn attaching_with_matching_versions_hands_over() {
        let tmux = cli(vec![ok("tmux 3.5a\n"), ok("3.5a\n")]);
        tmux.attach(&name()).unwrap();
        assert_eq!(tmux.handover_ref().calls().len(), 1);
    }

    #[test]
    fn doctor_fails_when_the_server_runs_another_tmux_version() {
        let tmux = cli(vec![ok("tmux 3.7c\n"), ok(""), ok("3.5a\n")]);
        let err = tmux.probe().unwrap_err();
        assert!(
            matches!(err, PortError::Incompatible { tool: "tmux", .. }),
            "got: {err:?}"
        );
        assert!(err.to_string().contains("3.5a"), "got: {err}");
    }

    #[test]
    fn doctor_passes_when_client_and_server_agree() {
        let tmux = cli(vec![ok("tmux 3.5a\n"), ok(""), ok("3.5a\n")]);
        let report = tmux.probe().unwrap();
        assert!(report.contains("server tmux 3.5a"), "got: {report}");
    }

    #[test]
    fn doctor_with_no_server_yet_has_no_version_to_compare() {
        let tmux = cli(vec![
            ok("tmux 3.7c\n"),
            failed(1, "no server running on /tmp/tmux-501/zax"),
        ]);
        tmux.probe().unwrap();
        assert_eq!(tmux.exec_ref().calls().len(), 2, "nothing to ask");
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
