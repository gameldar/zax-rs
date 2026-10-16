//! The subprocess seam shared by every shell-out adapter.
//!
//! A non-zero exit status is a value, not an error: git, tmux and gh all answer
//! questions that way. Only "the program could not be started" is an error.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use thiserror::Error;

pub mod askpass;
pub mod quote;

pub use quote::shell_quote;

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

/// One change to the environment a child would otherwise inherit whole.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvVar {
    Set(String, String),
    /// Removed outright rather than set empty: zax's own tmux guard treats an
    /// empty `TMUX` as unset, but not every program is so forgiving.
    Unset(String),
}

pub trait Exec {
    fn exec(
        &self,
        program: &str,
        args: &[String],
        cwd: Option<&Path>,
        env: &[EnvVar],
    ) -> Result<Output, ExecError>;
}

/// What a captured child is told, so that it never asks on the terminal.
///
/// Capturing a child's output means `/dev/null` for its stdin, but it does not
/// take away the terminal: a program that wants an answer opens `/dev/tty` and
/// writes the question straight past the pipes. In a pane that terminal belongs
/// to a TUI in raw mode, which paints over the question and eats the keystrokes
/// meant for it, so a prompt there is not a question at all - it is a hang with
/// no way out and nothing on stderr to say why.
///
/// `GIT_TERMINAL_PROMPT` and `SSH_ASKPASS_REQUIRE=force` are what move the
/// question off that terminal. Where it goes instead is `askpass`'s decision,
/// and the two are not the same thing: a locked ssh key with a helper to ask
/// through is a dialog, while one with nothing to ask through fails fast.
fn non_interactive() -> [(String, String); 4] {
    let display = has_display();
    let ssh = askpass::for_ssh(
        &askpass::Environment {
            configured: var("SSH_ASKPASS"),
            has_display: display,
        },
        &runnable,
    );
    let git = askpass::for_git(&askpass::Environment {
        configured: var("GIT_ASKPASS"),
        has_display: display,
    });

    [
        ("GIT_TERMINAL_PROMPT".to_owned(), "0".to_owned()),
        ("GIT_ASKPASS".to_owned(), git.program().to_owned()),
        ("SSH_ASKPASS".to_owned(), ssh.program().to_owned()),
        ("SSH_ASKPASS_REQUIRE".to_owned(), "force".to_owned()),
    ]
}

/// An environment variable the user set, where exported-but-empty counts as
/// unset: `askpass` treats both as "no choice made".
fn var(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|value| !value.is_empty())
}

/// Whether there is anywhere to draw a dialog. Without this an askpass found
/// on disk is a prompt that cannot appear, which is the hang all over again.
fn has_display() -> bool {
    ["DISPLAY", "WAYLAND_DISPLAY"]
        .iter()
        .any(|key| var(key).is_some())
}

/// A path that exists and can be executed. The distribution packages are
/// symlinks, so this follows them rather than asking about the link.
fn runnable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

/// A tool zax is asking a question of: git, gh, tmux, or another zax.
pub struct SystemExec;

impl Exec for SystemExec {
    fn exec(
        &self,
        program: &str,
        args: &[String],
        cwd: Option<&Path>,
        env: &[EnvVar],
    ) -> Result<Output, ExecError> {
        let mut command = StdCommand::new(program);
        // Before the caller's own entries, so a call with an opinion about one
        // of these still has it: this is the default for the calls that have
        // none, not a rule the adapters cannot get out of.
        for (key, value) in non_interactive() {
            command.env(key, value);
        }
        capture(configure(command, args, cwd, env), program)
    }
}

/// The askpass variables holding a helper zax invented, which are the only
/// ones it may take back.
///
/// Not setting a variable is not the same as the child not having it. A pane
/// runs `zax worktree review` through `SystemExec`, so that zax's own
/// environment carries the suppressions - and the editor it then launches
/// inherits them, which is the exception failing in exactly the place it was
/// written for. Only zax's invented path is removed: a helper the user chose,
/// or one found on this machine, is a real way of asking and stays.
fn fabricated(env: impl Fn(&str) -> Option<String>) -> Vec<&'static str> {
    ["GIT_ASKPASS", "SSH_ASKPASS"]
        .into_iter()
        .filter(|key| env(key).is_some_and(|value| askpass::is_nobody(&value)))
        .collect()
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
        env: &[EnvVar],
    ) -> Result<Output, ExecError> {
        let mut command = StdCommand::new(program);
        for key in fabricated(var) {
            command.env_remove(key);
        }
        capture(configure(command, args, cwd, env), program)
    }
}

fn configure(
    mut command: StdCommand,
    args: &[String],
    cwd: Option<&Path>,
    env: &[EnvVar],
) -> StdCommand {
    command.args(args);
    if let Some(dir) = cwd {
        command.current_dir(dir);
    }
    for var in env {
        match var {
            EnvVar::Set(key, value) => command.env(key, value),
            EnvVar::Unset(key) => command.env_remove(key),
        };
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
    pub env: Vec<EnvVar>,
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
        env: &[EnvVar],
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
                &[EnvVar::Set("ZAX_TEST".to_owned(), "set".to_owned())],
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
    fn a_captured_child_is_told_never_to_ask_on_the_terminal() {
        // The whole point of the policy: git must fail rather than ask there,
        // and ssh must be kept off the terminal even though it can open it.
        assert_eq!(sees(&SystemExec, "GIT_TERMINAL_PROMPT"), "0");
        assert_eq!(sees(&SystemExec, "SSH_ASKPASS_REQUIRE"), "force");
    }

    #[test]
    fn a_helper_counts_as_installed_only_when_it_can_actually_run() {
        // The bridge between the pure choice and the real filesystem: a path
        // that is merely present is not a helper, and a missing one is not
        // either.
        assert!(runnable(Path::new("/bin/sh")), "/bin/sh should be runnable");
        assert!(
            !runnable(Path::new("/etc/hostname")),
            "a plain file is not a helper"
        );
        assert!(!runnable(Path::new("/nonexistent/ssh-askpass")));
        assert!(!runnable(Path::new("/usr")), "a directory is not a helper");
    }

    #[test]
    fn a_captured_child_is_always_given_an_askpass_to_name() {
        // Which helper it is depends on the machine, so the invariant is only
        // that there is one: an unset `SSH_ASKPASS` is what sends ssh back to
        // the terminal, whatever `SSH_ASKPASS_REQUIRE` says.
        for variable in ["SSH_ASKPASS", "GIT_ASKPASS"] {
            assert!(
                !sees(&SystemExec, variable).is_empty(),
                "{variable} was not set"
            );
        }
    }

    #[test]
    fn a_captured_child_is_never_left_to_ask_on_the_terminal() {
        // The askpass either runs or cannot exist; either way the question
        // does not land on the terminal a pane's TUI is holding.
        let path = sees(&SystemExec, "SSH_ASKPASS");
        let path = Path::new(&path);

        assert!(
            runnable(path) || !path.exists(),
            "SSH_ASKPASS names something that exists but cannot run: {}",
            path.display()
        );
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
                &[EnvVar::Set(
                    "GIT_TERMINAL_PROMPT".to_owned(),
                    "1".to_owned(),
                )],
            )
            .unwrap();
        assert_eq!(out.stdout, "1");
    }

    #[test]
    fn a_program_launched_for_the_user_never_holds_the_invented_askpass() {
        // The property the old test missed: "inherited unchanged" is true of a
        // poisoned value too, so it passed for a parent zax that a pane had
        // captured. What matters is that the editor is never the one left
        // holding the helper that cannot run.
        for variable in ["GIT_ASKPASS", "SSH_ASKPASS"] {
            assert!(
                !askpass::is_nobody(&sees(&InteractiveExec, variable)),
                "{variable} still names the helper zax invented"
            );
        }
    }

    #[test]
    fn a_program_launched_for_the_user_is_given_no_suppressions_of_its_own() {
        // Taking zax's fabrication back is not licence to invent settings
        // either: everything else reaches the editor as it stands.
        for variable in ["GIT_TERMINAL_PROMPT", "SSH_ASKPASS_REQUIRE"] {
            assert_eq!(
                sees(&InteractiveExec, variable),
                std::env::var(variable).unwrap_or_default(),
                "{variable} was not inherited unchanged"
            );
        }
    }

    #[test]
    fn a_fabricated_askpass_is_taken_back_before_launching_for_the_user() {
        let nobody = askpass::Askpass::Nobody.program().to_owned();
        let env = |key: &str| match key {
            "GIT_ASKPASS" | "SSH_ASKPASS" => Some(nobody.clone()),
            _ => None,
        };

        assert_eq!(fabricated(env), ["GIT_ASKPASS", "SSH_ASKPASS"]);
    }

    #[test]
    fn a_helper_the_user_chose_is_never_taken_back() {
        // Indistinguishable from zax having set it, by value - which is why
        // only the invented path counts as zax's own.
        let env = |key: &str| match key {
            "SSH_ASKPASS" => Some("/usr/lib/openssh/ssh-askpass".to_owned()),
            _ => None,
        };

        assert!(fabricated(env).is_empty());
    }

    #[test]
    fn a_clean_environment_has_nothing_to_take_back() {
        assert!(fabricated(|_| None).is_empty());
    }

    #[test]
    fn the_system_runner_removes_an_unset_variable_from_the_child() {
        // HOME is set in any shell that runs the tests, so its absence in the
        // child can only be the unset.
        let out = SystemExec
            .exec(
                "sh",
                &["-c".to_owned(), "echo ${HOME-unset}".to_owned()],
                None,
                &[EnvVar::Unset("HOME".to_owned())],
            )
            .unwrap();
        assert_eq!(out.stdout.trim(), "unset");
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
