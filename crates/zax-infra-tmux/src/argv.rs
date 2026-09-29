//! Pure argv construction for tmux, always on the dedicated socket.

use std::path::Path;

use zax_domain::{Command, PaneSize, SessionName};

use crate::quote::shell_quote;

/// The socket and configuration every invocation carries.
#[derive(Debug, Clone, Copy)]
pub struct Base<'a> {
    pub socket: &'a str,
    pub conf: &'a str,
}

fn s(value: &str) -> String {
    value.to_owned()
}

fn start(base: Base<'_>) -> Vec<String> {
    vec![s("-L"), s(base.socket), s("-f"), s(base.conf)]
}

/// `=name` makes the target an exact match rather than a prefix match.
fn exact(name: &SessionName) -> String {
    format!("={}", name.as_str())
}

fn pane_target(name: &SessionName, pane: u32) -> String {
    format!("{}:0.{pane}", name.as_str())
}

pub fn version(base: Base<'_>) -> Vec<String> {
    let mut argv = start(base);
    argv.push(s("-V"));
    argv
}

pub fn has_session(base: Base<'_>, name: &SessionName) -> Vec<String> {
    let mut argv = start(base);
    argv.extend([s("has-session"), s("-t"), exact(name)]);
    argv
}

pub fn list_sessions(base: Base<'_>) -> Vec<String> {
    let mut argv = start(base);
    // The client count comes last: zax names cannot contain a space, but
    // somebody else's session on the socket can.
    argv.extend([
        s("list-sessions"),
        s("-F"),
        s("#{session_name} #{session_attached}"),
    ]);
    argv
}

pub fn new_session(
    base: Base<'_>,
    name: &SessionName,
    cwd: &Path,
    command: &Command,
) -> Vec<String> {
    let mut argv = start(base);
    argv.extend([
        s("new-session"),
        s("-d"),
        s("-s"),
        s(name.as_str()),
        s("-c"),
        cwd.to_string_lossy().into_owned(),
        shell_quote(command.argv()),
    ]);
    argv
}

pub fn split_window(
    base: Base<'_>,
    name: &SessionName,
    target_pane: u32,
    horizontal: bool,
    size: Option<PaneSize>,
    cwd: &Path,
    command: &Command,
) -> Vec<String> {
    let mut argv = start(base);
    argv.extend([
        s("split-window"),
        s("-t"),
        pane_target(name, target_pane),
        s(if horizontal { "-h" } else { "-v" }),
    ]);
    if let Some(size) = size {
        argv.extend([s("-l"), format!("{}%", size.get())]);
    }
    argv.extend([
        s("-c"),
        cwd.to_string_lossy().into_owned(),
        shell_quote(command.argv()),
    ]);
    argv
}

pub fn select_pane(base: Base<'_>, name: &SessionName, index: u32) -> Vec<String> {
    let mut argv = start(base);
    argv.extend([s("select-pane"), s("-t"), pane_target(name, index)]);
    argv
}

pub fn attach_session(base: Base<'_>, name: &SessionName) -> Vec<String> {
    let mut argv = start(base);
    argv.extend([s("attach-session"), s("-t"), exact(name)]);
    argv
}

pub fn kill_session(base: Base<'_>, name: &SessionName) -> Vec<String> {
    let mut argv = start(base);
    argv.extend([s("kill-session"), s("-t"), exact(name)]);
    argv
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use zax_domain::{BranchName, Command, PaneSize, RootRepoName, SessionName};

    use super::*;

    fn base() -> Base<'static> {
        Base {
            socket: "zax",
            conf: "/run/zax/tmux.conf",
        }
    }

    fn name() -> SessionName {
        SessionName::derive(
            &RootRepoName::parse("acme").unwrap(),
            &BranchName::parse_local("OMEGA-1963-stamp").unwrap(),
        )
    }

    /// The derived name as a string. These tests are about the argv tmux
    /// receives, not about how a session name is built, so the expectations are
    /// derived too. A hand-written literal here would fall out of step the
    /// moment `SessionName::derive` changes, and would fail in the tmux adapter
    /// rather than in the domain type that actually changed.
    fn n() -> String {
        name().as_str().to_owned()
    }

    #[test]
    fn every_command_runs_on_the_dedicated_socket_with_our_config() {
        let args = version(base());
        assert_eq!(&args[..4], ["-L", "zax", "-f", "/run/zax/tmux.conf"]);
    }

    #[test]
    fn session_lookup_uses_an_exact_match_target() {
        // Without `=`, tmux would match any session whose name starts with this.
        assert_eq!(
            has_session(base(), &name())[4..],
            [
                "has-session".to_owned(),
                "-t".to_owned(),
                format!("={}", n())
            ]
        );
    }

    #[test]
    fn a_new_session_is_created_detached_in_the_right_directory() {
        let cmd = Command::new(vec!["claude".to_owned()]).unwrap();
        assert_eq!(
            new_session(base(), &name(), std::path::Path::new("/w"), &cmd)[4..],
            [
                "new-session".to_owned(),
                "-d".to_owned(),
                "-s".to_owned(),
                n(),
                "-c".to_owned(),
                "/w".to_owned(),
                "claude".to_owned(),
            ]
        );
    }

    #[test]
    fn the_first_split_is_horizontal_and_sized() {
        let cmd = Command::new(vec!["zax".to_owned(), "pane".to_owned()]).unwrap();
        assert_eq!(
            split_window(
                base(),
                &name(),
                0,
                true,
                Some(PaneSize::percent(30).unwrap()),
                std::path::Path::new("/w"),
                &cmd,
            )[4..],
            [
                "split-window".to_owned(),
                "-t".to_owned(),
                format!("{}:0.0", n()),
                "-h".to_owned(),
                "-l".to_owned(),
                "30%".to_owned(),
                "-c".to_owned(),
                "/w".to_owned(),
                "zax pane".to_owned(),
            ]
        );
    }

    #[test]
    fn a_later_split_is_vertical_and_may_omit_the_size() {
        let cmd = Command::new(vec!["lazygit".to_owned()]).unwrap();
        assert_eq!(
            split_window(
                base(),
                &name(),
                2,
                false,
                None,
                std::path::Path::new("/w"),
                &cmd
            )[4..],
            [
                "split-window".to_owned(),
                "-t".to_owned(),
                format!("{}:0.2", n()),
                "-v".to_owned(),
                "-c".to_owned(),
                "/w".to_owned(),
                "lazygit".to_owned(),
            ]
        );
    }

    #[test]
    fn pane_selection_targets_window_zero() {
        assert_eq!(
            select_pane(base(), &name(), 0)[4..],
            [
                "select-pane".to_owned(),
                "-t".to_owned(),
                format!("{}:0.0", n())
            ]
        );
    }

    #[test]
    fn attach_kill_and_list_are_exact_and_scriptable() {
        assert_eq!(
            attach_session(base(), &name())[4..],
            [
                "attach-session".to_owned(),
                "-t".to_owned(),
                format!("={}", n())
            ]
        );
        assert_eq!(
            kill_session(base(), &name())[4..],
            [
                "kill-session".to_owned(),
                "-t".to_owned(),
                format!("={}", n())
            ]
        );
        assert_eq!(
            list_sessions(base())[4..],
            ["list-sessions", "-F", "#{session_name} #{session_attached}"]
        );
    }
}
