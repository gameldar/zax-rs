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
        command.args(args);
        if let Some(dir) = cwd {
            command.current_dir(dir);
        }
        for (key, value) in env {
            command.env(key, value);
        }

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
}
