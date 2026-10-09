//! `Editor` via whatever `[editor] command` names.
//!
//! The program is configuration rather than a fixed dependency, so this
//! adapter cannot name its tool the way the git and gh ones do. What it can
//! do is recognise the one program whose absence has a non-obvious cure, and
//! say what that cure is.

use std::path::{Path, PathBuf};

use zax_app::{Editor, PortError};
use zax_domain::Command;
use zax_exec::{Exec, ExecError};

/// On macOS the `zed` binary lives inside the application bundle, so a machine
/// with Zed installed and working still has no `zed` on PATH until this is
/// run. "not installed" on its own sends the reader looking for a download
/// they already have.
const ZED_HINT: &str = "if Zed is installed, open it and run `cli: install` \
                        from the command palette to put `zed` on PATH";

pub struct EditorCli<E: Exec> {
    exec: E,
    /// `None` is a configured decision to open nothing, not a missing value.
    command: Option<Command>,
}

impl<E: Exec> EditorCli<E> {
    pub fn new(exec: E, command: Option<Command>) -> Self {
        Self { exec, command }
    }

    /// For tests that assert on the recorded calls.
    pub fn exec_ref(&self) -> &E {
        &self.exec
    }

    /// Run the configured program with `args` appended to its configured ones.
    fn run(&self, command: &Command, args: &[String]) -> Result<(), PortError> {
        let program = command.program();
        let mut argv: Vec<String> = command.argv().iter().skip(1).cloned().collect();
        argv.extend_from_slice(args);

        let out = self
            .exec
            .exec(program, &argv, None, &[])
            .map_err(|e| map_exec_error(e, program))?;
        if out.ok() {
            return Ok(());
        }
        Err(failed(program, out.status, &out.stderr))
    }
}

impl<E: Exec> Editor for EditorCli<E> {
    fn open(&self, path: &Path) -> Result<(), PortError> {
        let Some(command) = &self.command else {
            return Ok(());
        };
        self.run(
            command,
            &[PathBuf::from(path).to_string_lossy().into_owned()],
        )
    }

    fn probe(&self) -> Result<String, PortError> {
        let Some(command) = &self.command else {
            return Ok("no editor configured, so a new worktree opens nothing".to_owned());
        };

        let program = command.program();
        // `--version` alone, not the configured flags: those are about opening
        // a path and a flag like `-e` with nothing to open is not a question
        // about whether the program is there.
        let out = self
            .exec
            .exec(program, &["--version".to_owned()], None, &[])
            .map_err(|e| map_exec_error(e, program))?;
        if !out.ok() {
            return Err(failed(program, out.status, &out.stderr));
        }
        Ok(out.stdout.lines().next().unwrap_or_default().to_owned())
    }
}

/// `PortError` wants a `&'static str` tool name and this program is whatever
/// the config said, so a configured program's failures carry their own.
fn failed(program: &str, status: i32, stderr: &str) -> PortError {
    PortError::Incompatible {
        tool: "editor",
        detail: format!("`{program}` exited with status {status}: {}", stderr.trim()),
    }
}

fn map_exec_error(error: ExecError, program: &str) -> PortError {
    match error {
        ExecError::NotFound { .. } => PortError::MissingProgram {
            program: program.to_owned(),
            hint: hint_for(program),
        },
        ExecError::Io { detail, .. } => PortError::Incompatible {
            tool: "editor",
            detail: format!("could not run `{program}`: {detail}"),
        },
    }
}

/// What to do about a program that is not on PATH. Only Zed has an answer zax
/// can give, because only Zed is the editor zax is built around.
fn hint_for(program: &str) -> String {
    if Path::new(program).file_name().is_some_and(|n| n == "zed") {
        ZED_HINT.to_owned()
    } else {
        format!("`{program}` comes from `[editor] command`; `zax config` is where to change it")
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use std::path::Path;

    use zax_app::{Editor, PortError};
    use zax_domain::Command;
    use zax_exec::{Exec, ExecError, Output, RecordingExec};

    use super::*;

    /// A program that is not on PATH. `RecordingExec` runs out of responses
    /// rather than failing to start, which is a different error.
    struct MissingExec;

    impl Exec for MissingExec {
        fn exec(
            &self,
            program: &str,
            _args: &[String],
            _cwd: Option<&Path>,
            _env: &[(String, String)],
        ) -> Result<Output, ExecError> {
            Err(ExecError::NotFound {
                program: program.to_owned(),
            })
        }
    }

    fn zed() -> Option<Command> {
        Some(Command::new(vec!["zed".to_owned(), "-e".to_owned()]).unwrap())
    }

    #[test]
    fn opening_appends_the_path_to_the_configured_argv() {
        let exec = RecordingExec::returning("");
        let editor = EditorCli::new(exec, zed());

        editor
            .open(Path::new("/Users/me/src/worktrees/acme/patch-1"))
            .unwrap();

        let call = &editor.exec_ref().calls()[0];
        assert_eq!(call.program, "zed");
        assert_eq!(
            call.args,
            ["-e", "/Users/me/src/worktrees/acme/patch-1"]
        );
    }

    #[test]
    fn opening_with_no_configured_command_runs_nothing_at_all() {
        // `editor.command = []` is how opening is turned off, and the type
        // already carries that: there is nothing left to decide here.
        let editor = EditorCli::new(RecordingExec::returning(""), None);

        editor.open(Path::new("/tmp/anywhere")).unwrap();

        assert!(editor.exec_ref().calls().is_empty());
    }

    #[test]
    fn a_probe_reports_the_editors_own_version_line() {
        let editor = EditorCli::new(RecordingExec::returning("Zed 0.147.2\n"), zed());

        let detail = editor.probe().unwrap();

        assert_eq!(detail, "Zed 0.147.2");
        assert_eq!(editor.exec_ref().args(0), ["--version"]);
    }

    #[test]
    fn a_probe_with_no_configured_command_is_fine_and_says_so() {
        let editor = EditorCli::new(RecordingExec::returning(""), None);

        let detail = editor.probe().unwrap();

        assert!(detail.contains("no editor"), "got: {detail}");
        assert!(editor.exec_ref().calls().is_empty());
    }

    #[test]
    fn a_missing_zed_says_how_a_mac_installs_the_cli() {
        // On macOS the `zed` binary ships inside the app bundle and is not on
        // PATH until it is installed from the app, so "not installed" is
        // usually wrong and unhelpful on its own.
        let editor = EditorCli::new(MissingExec, zed());

        let err = editor.probe().unwrap_err();

        assert!(matches!(err, PortError::MissingProgram { .. }));
        let message = err.to_string();
        assert!(message.contains("zed"), "got: {message}");
        assert!(message.contains("cli: install"), "got: {message}");
    }

    #[test]
    fn a_missing_editor_that_is_not_zed_gets_the_plain_answer() {
        let command = Command::new(vec!["hx".to_owned()]).unwrap();
        let editor = EditorCli::new(MissingExec, Some(command));

        let message = editor.probe().unwrap_err().to_string();

        assert!(message.contains("hx"), "got: {message}");
        assert!(!message.contains("cli: install"), "got: {message}");
    }

    #[test]
    fn an_editor_that_refuses_to_open_the_path_is_an_error_not_a_shrug() {
        let exec = RecordingExec::with_responses(vec![Output {
            status: 1,
            stdout: String::new(),
            stderr: "no such file or directory".to_owned(),
        }]);
        let editor = EditorCli::new(exec, zed());

        let message = editor.open(Path::new("/tmp/gone")).unwrap_err().to_string();

        assert!(message.contains("no such file"), "got: {message}");
    }

    #[test]
    fn a_probe_whose_editor_has_no_version_flag_still_fails_with_its_own_words() {
        let exec = RecordingExec::with_responses(vec![Output {
            status: 64,
            stdout: String::new(),
            stderr: "unknown option --version".to_owned(),
        }]);
        let editor = EditorCli::new(exec, zed());

        let message = editor.probe().unwrap_err().to_string();

        assert!(message.contains("unknown option"), "got: {message}");
    }

    #[test]
    fn a_missing_editor_is_reported_when_it_is_asked_to_open_something_too() {
        // The `w` row action is the path that matters, and it never calls
        // probe: the missing program has to be an error there as well.
        let editor = EditorCli::new(MissingExec, zed());

        let err = editor.open(Path::new("/tmp/anywhere")).unwrap_err();

        assert!(matches!(err, PortError::MissingProgram { .. }));
    }

    #[test]
    fn an_editor_named_by_an_absolute_path_is_still_recognised_as_zed() {
        // A config can perfectly well point straight at the binary in the
        // app bundle, and the hint is about the program, not how it is spelt.
        let command =
            Command::new(vec!["/Applications/Zed.app/Contents/MacOS/zed".to_owned()]).unwrap();
        let editor = EditorCli::new(MissingExec, Some(command));

        let message = editor.probe().unwrap_err().to_string();

        assert!(message.contains("cli: install"), "got: {message}");
    }
}
