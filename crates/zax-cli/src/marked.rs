//! How zax marks its own lines on stderr, and how a parent that captured them
//! reads them back.
//!
//! A pane runs `zax` as a subprocess and sees nothing but its exit status and
//! its output, so a line that has to be found again only exists if it was
//! marked. Setup hooks inherit the same stderr and the tools zax drives write
//! to it too, which is why "something on stderr" cannot be the signal.
//!
//! Both halves of the convention live here. The marking and the search drifting
//! apart is how a warning goes missing, and zax's own error line being spelled
//! in one file and looked for in another is how a pane came to show git's
//! closing advice instead of the reason a fetch failed.

/// Every line zax writes about itself starts here.
const PREFIX: &str = "zax: ";

/// A warning is the same voice saying something less final than an error.
const WARNING: &str = "zax: warning: ";

/// The marked first line, then any continuations indented under it so they do
/// not read as warnings of their own.
fn warning_lines(message: &str) -> Vec<String> {
    let mut lines = message.lines();
    let first = format!("{WARNING}{}", lines.next().unwrap_or_default());
    std::iter::once(first)
        .chain(lines.map(|line| format!("  {}", line.trim_start())))
        .collect()
}

pub fn warn(message: &str) {
    for line in warning_lines(message) {
        eprintln!("{line}");
    }
}

/// How zax reports the error that ended a command.
pub fn fail(message: &str) {
    eprintln!("{PREFIX}{message}");
}

/// The warning zax wrote, if it wrote one. Only the marked line: it is the one
/// that names what went wrong, and a caller with one line to spend wants that
/// one rather than the advice indented under it.
pub fn find_warning(stderr: &str) -> Option<String> {
    stderr
        .lines()
        .find_map(|line| line.trim_start().strip_prefix(WARNING))
        .map(str::trim_end)
        .map(str::to_owned)
}

/// The error that ended the command, as zax spelled it.
///
/// First rather than last. zax prints its whole chain on one line, but the
/// tool's stderr inside that chain brings its own newlines, so the lines after
/// it belong to the tool - and git ends a remote failure with `Please make
/// sure you have the correct access rights` every time. The last line of
/// stderr is therefore the one line that is reliably not the reason.
pub fn find_error(stderr: &str) -> Option<String> {
    stderr
        .lines()
        .map(str::trim_start)
        .filter(|line| !line.starts_with(WARNING))
        .find_map(|line| line.strip_prefix(PREFIX))
        .map(str::trim_end)
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;

    #[test]
    fn a_warning_zax_printed_is_a_warning_zax_can_find_again() {
        // The round trip is the whole point: the pane reads what the command
        // wrote, and the two halves drifting apart is how a warning goes
        // missing.
        let printed = warning_lines("could not open the editor: zed is not on PATH").join("\n");

        assert_eq!(
            find_warning(&printed).unwrap(),
            "could not open the editor: zed is not on PATH"
        );
    }

    #[test]
    fn advice_is_indented_under_the_warning_rather_than_repeating_it() {
        let printed =
            warning_lines("could not open the editor: zed is not on PATH\nrun `cli: install`");

        assert_eq!(printed[1], "  run `cli: install`");
        assert_eq!(find_warning(&printed.join("\n")).unwrap(), printed[0][14..]);
    }

    #[test]
    fn output_nobody_marked_is_not_a_warning() {
        // A hook's chatter arrives on the same stderr.
        assert_eq!(
            find_warning("npm warn deprecated glob@7\nwarning: noise\n"),
            None
        );
    }

    #[test]
    fn the_error_is_the_marked_line_and_not_the_tools_last_word() {
        // The failure that sent this looking: git's closing advice is the last
        // line of every remote failure, and it is never the reason.
        let stderr = "zax: git exited with status 128: Permission denied (publickey).\n\
                      fatal: Could not read from remote repository.\n\n\
                      Please make sure you have the correct access rights\n\
                      and the repository exists.\n";

        assert_eq!(
            find_error(stderr).unwrap(),
            "git exited with status 128: Permission denied (publickey)."
        );
    }

    #[test]
    fn a_warning_is_not_mistaken_for_the_error_that_ended_the_command() {
        // Both start with `zax: `, and a command can warn and then fail.
        let stderr = "zax: warning: could not open the worktree in the editor: no zed\n\
                      zax: git exited with status 1: no such ref\n";

        assert_eq!(
            find_error(stderr).unwrap(),
            "git exited with status 1: no such ref"
        );
    }

    #[test]
    fn output_nobody_marked_carries_no_error_either() {
        assert_eq!(find_error("fatal: not a git repository\n"), None);
    }
}
