//! What zax can say about the tmux it just failed to drive.
//!
//! A bare tmux stderr is often too terse to act on. "server exited
//! unexpectedly" names neither the command that failed, nor the socket it used,
//! nor the tmux that ran it, and one of those three is usually the answer.

use std::path::{Path, PathBuf};

/// tmux serves only clients built from its own version. A client of a
/// different build is disconnected, which the client reports as the server
/// exiting. Because the server outlives any single zax invocation by design,
/// a change to `PATH` between two invocations produces this with nothing else
/// having changed, and it persists until that server is killed.
pub fn mismatch_hint(socket: &str) -> String {
    format!(
        "a tmux server is already running on socket \"{socket}\" under a \
different tmux build, which refuses clients of another version. Find it with \
`pgrep -fl 'tmux -L {socket}'`, then either use that same tmux binary or kill \
the server with it (`<that-tmux> -L {socket} kill-server`) to start fresh."
    )
}

/// The tmux `display-popup` the config binding uses, with the border style it
/// asks for, arrived in 3.3.
const POPUP_SINCE: (u32, u32) = (3, 3);

/// What to say about a tmux too old for the config popup, given a version
/// string as `tmux -V` prints it (`tmux 3.2a`) or as `zax doctor` renders it.
///
/// Below this version `prefix C` reports an unknown command on tmux's message
/// line rather than opening anything, which is a confusing way to learn that the
/// multiplexer is the problem. `zax config` itself is unaffected: it is a
/// command, and it runs in a pane like any other.
pub fn popup_hint(version: &str) -> Option<String> {
    let (major, minor) = tmux_version(version)?;
    if (major, minor) >= POPUP_SINCE {
        return None;
    }
    Some(format!(
        "tmux {major}.{minor} has no `display-popup` with border styles, so `prefix C` will report an error instead of opening the config editor. Run `zax config` in a pane instead, or upgrade to tmux {}.{}.",
        POPUP_SINCE.0, POPUP_SINCE.1
    ))
}

/// The major and minor numbers out of whatever tmux and zax print around them.
///
/// tmux versions carry a letter suffix (`3.2a`) and `next-3.4` exists too, so
/// the digits are taken as they come and anything else is ignored rather than
/// refused: a version zax cannot read must not produce a warning about a
/// capability it knows nothing about.
fn tmux_version(text: &str) -> Option<(u32, u32)> {
    let rest = text.split("tmux ").nth(1).unwrap_or(text).trim();
    let digits = rest.trim_start_matches(|c: char| !c.is_ascii_digit());
    let mut parts = digits.split('.');
    let major: u32 = parts.next()?.parse().ok()?;
    let minor_text: String = parts
        .next()?
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    let minor: u32 = minor_text.parse().ok()?;
    Some((major, minor))
}

/// Whether this stderr is one zax can explain further. The socket is named
/// because the recovery commands are useless without it.
pub fn hint_for(stderr: &str, socket: &str) -> Option<String> {
    let stderr = stderr.to_ascii_lowercase();
    let mismatched = stderr.contains("server exited unexpectedly")
        || stderr.contains("protocol version mismatch");
    mismatched.then(|| mismatch_hint(socket))
}

/// Find `program` the way a shell would, given a `PATH` value.
///
/// `is_executable` is a parameter so the search itself stays pure and
/// testable; the caller supplies the filesystem.
pub fn resolve_on_path(
    program: &str,
    path_var: &str,
    is_executable: &dyn Fn(&Path) -> bool,
) -> Option<PathBuf> {
    // A program with a separator is a path already, not something to search
    // for, which is how a shell treats it too.
    if program.contains('/') {
        let direct = PathBuf::from(program);
        return is_executable(&direct).then_some(direct);
    }
    path_var
        .split(':')
        .filter(|dir| !dir.is_empty())
        .map(|dir| Path::new(dir).join(program))
        .find(|candidate| is_executable(candidate))
}

/// The environment of a single failed tmux invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostics {
    pub argv: Vec<String>,
    pub socket: String,
    pub conf: PathBuf,
    pub binary: Option<PathBuf>,
    pub version: Option<String>,
    pub hint: Option<String>,
}

impl Diagnostics {
    /// Render as indented lines, one fact per line, omitting what is unknown.
    /// An absent fact says nothing rather than saying "unknown": a line that
    /// carries no information is a line the reader has to discard.
    pub fn render(&self) -> String {
        let mut lines = Vec::new();
        if !self.argv.is_empty() {
            lines.push(format!("  while running: tmux {}", self.argv.join(" ")));
        }
        lines.push(format!("  socket:        {}", self.socket));
        lines.push(format!("  config:        {}", self.conf.display()));
        match (&self.version, &self.binary) {
            (Some(version), Some(binary)) => {
                lines.push(format!(
                    "  tmux:          {version} at {}",
                    binary.display()
                ));
            }
            (Some(version), None) => lines.push(format!("  tmux:          {version}")),
            (None, Some(binary)) => {
                lines.push(format!("  tmux:          {}", binary.display()));
            }
            (None, None) => {}
        }
        if let Some(hint) = &self.hint {
            lines.push(format!("  hint:          {hint}"));
        }
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;

    fn diagnostics() -> Diagnostics {
        Diagnostics {
            argv: vec![
                "-L".to_owned(),
                "zax".to_owned(),
                "attach-session".to_owned(),
            ],
            socket: "zax".to_owned(),
            conf: PathBuf::from("/home/me/.cache/zax/tmux.conf"),
            binary: Some(PathBuf::from("/usr/bin/tmux")),
            version: Some("tmux 3.5a".to_owned()),
            hint: None,
        }
    }

    #[test]
    fn the_rendered_block_names_the_command_socket_config_and_binary() {
        let rendered = diagnostics().render();
        assert!(rendered.contains("while running: tmux -L zax attach-session"));
        assert!(rendered.contains("socket:        zax"));
        assert!(rendered.contains("config:        /home/me/.cache/zax/tmux.conf"));
        assert!(rendered.contains("tmux:          tmux 3.5a at /usr/bin/tmux"));
    }

    #[test]
    fn a_fact_zax_does_not_know_produces_no_line_at_all() {
        let mut d = diagnostics();
        d.binary = None;
        d.version = None;
        let rendered = d.render();
        assert!(!rendered.contains("tmux:"), "rendered was:\n{rendered}");
        // The facts it does know are still there.
        assert!(rendered.contains("socket:        zax"));
    }

    #[test]
    fn a_tmux_without_popups_earns_a_line_about_the_config_binding() {
        // `prefix C` is useless on an old tmux, and the error it produces says
        // nothing about tmux being the reason.
        let hint = popup_hint("tmux 3.2a").expect("3.2 has no styled popup");
        assert!(hint.contains("prefix C"), "got: {hint}");
        assert!(hint.contains("zax config"), "got: {hint}");
        assert!(hint.contains("3.3"), "got: {hint}");
    }

    #[test]
    fn a_tmux_new_enough_for_the_popup_says_nothing() {
        for version in [
            "tmux 3.3",
            "tmux 3.3a",
            "tmux 3.5a",
            "tmux 4.0",
            "tmux next-3.4",
        ] {
            assert_eq!(popup_hint(version), None, "{version} is new enough");
        }
    }

    #[test]
    fn a_version_string_zax_cannot_read_produces_no_claim_about_popups() {
        for version in ["", "tmux", "tmux unknown", "master"] {
            assert_eq!(popup_hint(version), None, "{version:?}");
        }
    }

    #[test]
    fn the_hint_is_found_in_the_line_doctor_renders_too() {
        // `zax doctor` prints the version inside a longer line, and that is the
        // string the command has to hand.
        let detail = "tmux 3.2a at /usr/bin/tmux; socket \"zax\": no server";
        assert!(popup_hint(detail).is_some(), "got: {detail}");
    }

    #[test]
    fn the_version_mismatch_stderr_earns_a_hint_naming_the_socket() {
        let hint = hint_for("server exited unexpectedly", "zax").unwrap();
        assert!(hint.contains("different tmux build"), "got: {hint}");
        // The recovery commands are useless without the socket name in them.
        assert!(hint.contains("pgrep -fl 'tmux -L zax'"), "got: {hint}");
        assert!(hint.contains("-L zax kill-server"), "got: {hint}");
        assert!(hint_for("protocol version mismatch (client 8, server 7)", "zax").is_some());
        // Matching is case-insensitive because the spelling is not ours.
        assert!(hint_for("Server Exited Unexpectedly", "zax").is_some());
    }

    #[test]
    fn a_hint_names_whichever_socket_it_was_given() {
        let hint = hint_for("server exited unexpectedly", "other").unwrap();
        assert!(hint.contains("'tmux -L other'"), "got: {hint}");
        assert!(!hint.contains("-L zax"), "got: {hint}");
    }

    #[test]
    fn an_ordinary_failure_earns_no_hint() {
        assert_eq!(hint_for("can't find session: nope", "zax"), None);
        assert_eq!(hint_for("duplicate session: zax-a-b", "zax"), None);
    }

    #[test]
    fn a_program_is_found_in_the_first_path_entry_that_holds_it() {
        let found = resolve_on_path("tmux", "/nope:/usr/bin:/usr/local/bin", &|p: &Path| {
            p == Path::new("/usr/bin/tmux") || p == Path::new("/usr/local/bin/tmux")
        });
        assert_eq!(found, Some(PathBuf::from("/usr/bin/tmux")));
    }

    #[test]
    fn an_empty_path_entry_is_skipped_rather_than_probed_as_the_root() {
        // "::" in PATH would otherwise produce the candidate "/tmux". Accept
        // anything, so the only way to return "/tmux" is to have probed it.
        assert_eq!(
            resolve_on_path("tmux", "::/usr/bin", &|_: &Path| true),
            Some(PathBuf::from("/usr/bin/tmux"))
        );
        assert_eq!(resolve_on_path("tmux", "", &|_: &Path| true), None);
    }

    #[test]
    fn a_program_that_is_already_a_path_is_not_searched_for() {
        let found = resolve_on_path("/opt/tmux/bin/tmux", "/usr/bin", &|p: &Path| {
            p == Path::new("/opt/tmux/bin/tmux")
        });
        assert_eq!(found, Some(PathBuf::from("/opt/tmux/bin/tmux")));
        // And when it is not executable, PATH is not consulted as a fallback.
        assert_eq!(
            resolve_on_path("/opt/tmux/bin/tmux", "/usr/bin", &|_: &Path| false),
            None
        );
    }

    #[test]
    fn nothing_on_the_path_is_an_absent_answer_not_a_guess() {
        assert_eq!(resolve_on_path("tmux", "/a:/b", &|_: &Path| false), None);
    }
}
