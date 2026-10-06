//! How zax marks a warning on stderr, and how a parent that captured it reads
//! it back.
//!
//! A pane runs `zax` as a subprocess and sees nothing but its exit status and
//! its output, so a warning that exits zero only exists if it can be found
//! again. Setup hooks inherit the same stderr, which is why "something on
//! stderr" cannot be the signal and the marker has to be.

/// `zax: ` is already how zax spells its own errors on stderr; a warning is
/// the same voice saying something less final.
const PREFIX: &str = "zax: warning: ";

/// The marked first line, then any continuations indented under it so they do
/// not read as warnings of their own.
fn lines(message: &str) -> Vec<String> {
    let mut lines = message.lines();
    let first = format!("{PREFIX}{}", lines.next().unwrap_or_default());
    std::iter::once(first)
        .chain(lines.map(|line| format!("  {}", line.trim_start())))
        .collect()
}

pub fn print(message: &str) {
    for line in lines(message) {
        eprintln!("{line}");
    }
}

/// The warning zax wrote, if it wrote one. Only the marked line: it is the one
/// that names what went wrong, and a caller with one line to spend wants that
/// one rather than the advice indented under it.
pub fn find(stderr: &str) -> Option<String> {
    stderr
        .lines()
        .find_map(|line| line.trim_start().strip_prefix(PREFIX))
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
        let printed = lines("could not open the editor: zed is not on PATH").join("\n");

        assert_eq!(
            find(&printed).unwrap(),
            "could not open the editor: zed is not on PATH"
        );
    }

    #[test]
    fn advice_is_indented_under_the_warning_rather_than_repeating_it() {
        let printed = lines("could not open the editor: zed is not on PATH\nrun `cli: install`");

        assert_eq!(printed[1], "  run `cli: install`");
        assert_eq!(find(&printed.join("\n")).unwrap(), printed[0][14..]);
    }

    #[test]
    fn output_nobody_marked_is_not_a_warning() {
        // A hook's chatter arrives on the same stderr.
        assert_eq!(find("npm warn deprecated glob@7\nwarning: noise\n"), None);
    }
}
