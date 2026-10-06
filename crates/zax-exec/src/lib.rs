//! The subprocess seam shared by every shell-out adapter.
//!
//! A non-zero exit status is a value, not an error: git, tmux and gh all answer
//! questions that way. Only "the program could not be started" is an error.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    pub fn ok(&self) -> bool {
        self.status == 0
    }

    pub fn trimmed_stdout(&self) -> &str {
        self.stdout.trim_end_matches(['\n', '\r'])
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ExecError {
    #[error("{program} is not installed or not on PATH")]
    NotFound { program: String },
    #[error("could not run {program}: {detail}")]
    Io { program: String, detail: String },
}

pub trait Exec {
    fn exec(
        &self,
        program: &str,
        args: &[String],
        cwd: Option<&Path>,
        env: &[(String, String)],
    ) -> Result<Output, ExecError>;
}

/// Named rather than installed, because it has to be a program that cannot be
/// run. An askpass that might exist one day is a prompt waiting to come back.
const NO_ASKPASS: &str = "/nonexistent/zax-has-nobody-to-ask";

/// What a captured child is told, so that it fails rather than asks.
///
/// Capturing a child's output means `/dev/null` for its stdin, but it does not
/// take away the terminal: a program that wants an answer opens `/dev/tty` and
/// writes the question straight past the pipes. In a pane that terminal belongs
/// to a TUI in raw mode, which paints over the question and eats the keystrokes
/// meant for it, so a prompt there is not a question at all - it is a hang with
/// no way out and nothing on stderr to say why.
///
/// `GIT_TERMINAL_PROMPT` covers git's own credential prompts. ssh is the harder
/// one, because it reads from `/dev/tty` whether or not an askpass program
/// exists: only `SSH_ASKPASS_REQUIRE=force` moves it off the terminal, and
/// pointing it at a helper that cannot run is what leaves it nowhere to ask at
/// all. It then fails the way an unusable key always fails, which is a line the
/// adapters already know how to report.
const NON_INTERACTIVE: [(&str, &str); 4] = [
    ("GIT_TERMINAL_PROMPT", "0"),
    ("GIT_ASKPASS", NO_ASKPASS),
    ("SSH_ASKPASS", NO_ASKPASS),
    ("SSH_ASKPASS_REQUIRE", "force"),
];

/// A tool zax is asking a question of: git, gh, tmux, or another zax.
pub struct SystemExec;

impl Exec for SystemExec {
    fn exec(
        &self,
        program: &str,
        args: &[String],
        cwd: Option<&Path>,
        env: &[(String, String)],
    ) -> Result<Output, ExecError> {
        let mut command = StdCommand::new(program);
        // Before the caller's own entries, so a call with an opinion about one
        // of these still has it: this is the default for the calls that have
        // none, not a rule the adapters cannot get out of.
        for (key, value) in NON_INTERACTIVE {
            command.env(key, value);
        }
        capture(configure(command, args, cwd, env), program)
    }
}

/// A program started on the user's behalf rather than questioned by zax, and
/// so one with a user of its own to ask.
///
/// The difference from `SystemExec` is only the prompting policy, and it is
/// the whole reason this exists: an editor launched with zax's suppressions
/// in its environment loses the credential dialog it has of its own, and a
/// GUI program denied every way of asking is a window that quietly fails.
pub struct InteractiveExec;

impl Exec for InteractiveExec {
    fn exec(
        &self,
        program: &str,
        args: &[String],
        cwd: Option<&Path>,
        env: &[(String, String)],
    ) -> Result<Output, ExecError> {
        capture(configure(StdCommand::new(program), args, cwd, env), program)
    }
}

fn configure(
    mut command: StdCommand,
    args: &[String],
    cwd: Option<&Path>,
    env: &[(String, String)],
) -> StdCommand {
    command.args(args);
    if let Some(dir) = cwd {
        command.current_dir(dir);
    }
    for (key, value) in env {
        command.env(key, value);
    }
    command
}

fn capture(mut command: StdCommand, program: &str) -> Result<Output, ExecError> {
    let output = command.output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ExecError::NotFound {
                program: program.to_owned(),
            }
        } else {
            ExecError::Io {
                program: program.to_owned(),
                detail: e.to_string(),
            }
        }
    })?;

    Ok(Output {
        // A signal-killed child has no code; -1 stands for "did not exit
        // normally", which every caller treats as a failure anyway.
        status: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecCall {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
}

/// Replays canned responses in order and remembers every call, so an adapter
/// test can assert the exact command line it built.
pub struct RecordingExec {
    calls: RefCell<Vec<ExecCall>>,
    responses: RefCell<VecDeque<Output>>,
}

impl RecordingExec {
    pub fn with_responses(responses: Vec<Output>) -> Self {
        Self {
            calls: RefCell::new(Vec::new()),
            responses: RefCell::new(responses.into()),
        }
    }

    /// One successful response with this stdout.
    pub fn returning(stdout: &str) -> Self {
        Self::with_responses(vec![Output {
            status: 0,
            stdout: stdout.to_owned(),
            stderr: String::new(),
        }])
    }

    pub fn calls(&self) -> Vec<ExecCall> {
        self.calls.borrow().clone()
    }

    /// The args of call `index`, for a concise assertion.
    pub fn args(&self, index: usize) -> Vec<String> {
        self.calls
            .borrow()
            .get(index)
            .map(|c| c.args.clone())
            .unwrap_or_default()
    }
}

impl Exec for RecordingExec {
    fn exec(
        &self,
        program: &str,
        args: &[String],
        cwd: Option<&Path>,
        env: &[(String, String)],
    ) -> Result<Output, ExecError> {
        self.calls.borrow_mut().push(ExecCall {
            program: program.to_owned(),
            args: args.to_vec(),
            cwd: cwd.map(Path::to_owned),
            env: env.to_vec(),
        });
        self.responses
            .borrow_mut()
            .pop_front()
            .ok_or_else(|| ExecError::Io {
                program: program.to_owned(),
                detail: format!("RecordingExec has no response left for {args:?}"),
            })
    }
}

/// Handing the terminal over to another program.
///
/// This is not `Exec`: attaching to a multiplexer needs the real terminal, not
/// a captured pipe, and on success the current process ceases to exist.
pub trait Handover {
    /// Replace the current process. Returns only on failure.
    fn handover(&self, program: &str, args: &[String]) -> Result<(), ExecError>;
}

pub struct SystemHandover;

impl Handover for SystemHandover {
    fn handover(&self, program: &str, args: &[String]) -> Result<(), ExecError> {
        use std::os::unix::process::CommandExt;

        // `exec` returns only if it failed.
        let error = StdCommand::new(program).args(args).exec();
        Err(if error.kind() == std::io::ErrorKind::NotFound {
            ExecError::NotFound {
                program: program.to_owned(),
            }
        } else {
            ExecError::Io {
                program: program.to_owned(),
                detail: error.to_string(),
            }
        })
    }
}

#[derive(Default)]
pub struct RecordingHandover {
    calls: RefCell<Vec<(String, Vec<String>)>>,
}

impl RecordingHandover {
    pub fn calls(&self) -> Vec<(String, Vec<String>)> {
        self.calls.borrow().clone()
    }
}

impl Handover for RecordingHandover {
    fn handover(&self, program: &str, args: &[String]) -> Result<(), ExecError> {
        self.calls
            .borrow_mut()
            .push((program.to_owned(), args.to_vec()));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use std::path::Path;

    use super::*;

    #[test]
    fn the_system_runner_captures_stdout_and_status() {
        let out = SystemExec
            .exec("sh", &["-c".to_owned(), "echo hello".to_owned()], None, &[])
            .unwrap();
        assert_eq!(out.status, 0);
        assert_eq!(out.stdout.trim(), "hello");
    }

    #[test]
    fn a_nonzero_exit_is_a_value_not_an_error() {
        // git and tmux both use exit status as an answer, not a failure.
        let out = SystemExec
            .exec("sh", &["-c".to_owned(), "exit 3".to_owned()], None, &[])
            .unwrap();
        assert_eq!(out.status, 3);
    }

    #[test]
    fn a_missing_program_is_reported_as_not_found() {
        let err = SystemExec
            .exec("definitely-not-a-real-program-zax", &[], None, &[])
            .unwrap_err();
        assert!(matches!(err, ExecError::NotFound { .. }));
    }

    #[test]
    fn the_system_runner_honours_cwd_and_env() {
        let out = SystemExec
            .exec(
                "sh",
                &["-c".to_owned(), "pwd; echo $ZAX_TEST".to_owned()],
                Some(Path::new("/")),
                &[("ZAX_TEST".to_owned(), "set".to_owned())],
            )
            .unwrap();
        let lines: Vec<&str> = out.stdout.lines().collect();
        assert_eq!(lines, ["/", "set"], "cwd then env; got: {:?}", out.stdout);
    }

    /// `sh` reporting one variable, so the child's view of its own environment
    /// is what is asserted rather than this process's.
    fn sees(exec: &dyn Exec, variable: &str) -> String {
        exec.exec(
            "sh",
            &["-c".to_owned(), format!("printf '%s' \"${{{variable}-}}\"")],
            None,
            &[],
        )
        .unwrap()
        .stdout
    }

    #[test]
    fn a_captured_child_is_told_that_nobody_can_answer_a_prompt() {
        // The whole point of the policy: git must fail rather than ask, and
        // ssh must be kept off the terminal even though it can still open it.
        assert_eq!(sees(&SystemExec, "GIT_TERMINAL_PROMPT"), "0");
        assert_eq!(sees(&SystemExec, "SSH_ASKPASS_REQUIRE"), "force");
    }

    #[test]
    fn the_askpass_a_captured_child_is_given_cannot_be_run() {
        // `SSH_ASKPASS_REQUIRE=force` only moves the question off the terminal;
        // it is an unrunnable helper that leaves it with nowhere to go at all.
        for variable in ["SSH_ASKPASS", "GIT_ASKPASS"] {
            let path = sees(&SystemExec, variable);
            assert!(!path.is_empty(), "{variable} was not set");
            assert!(
                !Path::new(&path).exists(),
                "{variable} names something that exists: {path}"
            );
        }
    }

    #[test]
    fn a_caller_that_names_one_of_those_variables_still_wins() {
        // The policy is a default for the calls that have no opinion, not a
        // rule the adapters cannot get out of.
        let out = SystemExec
            .exec(
                "sh",
                &[
                    "-c".to_owned(),
                    "printf '%s' \"$GIT_TERMINAL_PROMPT\"".to_owned(),
                ],
                None,
                &[("GIT_TERMINAL_PROMPT".to_owned(), "1".to_owned())],
            )
            .unwrap();
        assert_eq!(out.stdout, "1");
    }

    #[test]
    fn a_program_launched_for_the_user_is_left_to_ask_its_own_way() {
        // An editor is handed to the user, not interrogated by zax: suppressing
        // its prompts would disable the credential dialog it has of its own.
        for variable in ["GIT_TERMINAL_PROMPT", "SSH_ASKPASS_REQUIRE", "SSH_ASKPASS"] {
            assert_eq!(
                sees(&InteractiveExec, variable),
                std::env::var(variable).unwrap_or_default(),
                "{variable} was not inherited unchanged"
            );
        }
    }

    #[test]
    fn the_recording_runner_replays_responses_and_remembers_calls() {
        let exec = RecordingExec::with_responses(vec![
            Output {
                status: 0,
                stdout: "first".to_owned(),
                stderr: String::new(),
            },
            Output {
                status: 1,
                stdout: String::new(),
                stderr: "boom".to_owned(),
            },
        ]);

        assert_eq!(
            exec.exec("git", &["a".to_owned()], None, &[])
                .unwrap()
                .stdout,
            "first"
        );
        assert_eq!(
            exec.exec("git", &["b".to_owned()], None, &[])
                .unwrap()
                .status,
            1
        );

        assert_eq!(
            exec.calls(),
            vec![
                ExecCall {
                    program: "git".to_owned(),
                    args: vec!["a".to_owned()],
                    cwd: None,
                    env: vec![],
                },
                ExecCall {
                    program: "git".to_owned(),
                    args: vec!["b".to_owned()],
                    cwd: None,
                    env: vec![],
                },
            ]
        );
    }

    #[test]
    fn the_recording_runner_errors_when_it_runs_out_of_responses() {
        let exec = RecordingExec::with_responses(vec![]);
        assert!(exec.exec("git", &[], None, &[]).is_err());
    }

    #[test]
    fn the_recording_handover_remembers_what_it_was_asked_to_become() {
        let handover = RecordingHandover::default();
        handover
            .handover("tmux", &["attach-session".to_owned(), "-t".to_owned()])
            .unwrap();
        assert_eq!(
            handover.calls(),
            vec![(
                "tmux".to_owned(),
                vec!["attach-session".to_owned(), "-t".to_owned()]
            )]
        );
    }

    #[test]
    fn the_system_handover_reports_a_missing_program_rather_than_replacing_the_process() {
        let err = SystemHandover
            .handover("definitely-not-a-real-program-zax", &[])
            .unwrap_err();
        assert!(matches!(err, ExecError::NotFound { .. }));
    }
}
