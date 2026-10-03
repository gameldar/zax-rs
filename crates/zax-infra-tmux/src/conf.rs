//! Rendering and writing zax's own tmux configuration.

use std::path::{Path, PathBuf};

pub const DEFAULT_CONF_TEMPLATE: &str = include_str!("../assets/tmux.conf");

/// Substitute the configured prefix key and this build's own binary into the
/// template.
///
/// `zax` is a parameter rather than the literal word `zax` because the config
/// popup has to run the binary this session was started from, exactly as the
/// panes do. A `zax` resolved from `PATH` inside a popup is whatever happens to
/// be installed, which on a machine mid-upgrade is a different build.
pub fn render(prefix: &str, zax: &Path) -> String {
    DEFAULT_CONF_TEMPLATE
        .replace("{{PREFIX}}", prefix)
        .replace("{{ZAX}}", &shell_word(zax))
}

/// The binary path as one shell word, inside the single quotes tmux wraps the
/// popup command in.
///
/// tmux hands the command to a shell, which splits it on spaces, so a path
/// containing one has to carry its own quoting. Double quotes rather than
/// single: the tmux side of this string is already single-quoted, and there is
/// no way to nest those.
fn shell_word(path: &Path) -> String {
    let text = path.to_string_lossy();
    let safe = !text.is_empty()
        && text.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | '+' | ':' | '@' | '%')
        });
    if safe {
        return text.into_owned();
    }
    let escaped: String = text
        .chars()
        .flat_map(|c| {
            // Everything a double-quoted shell word still interprets.
            let escape = matches!(c, '"' | '\\' | '$' | '`');
            escape.then_some('\\').into_iter().chain(std::iter::once(c))
        })
        .collect();
    format!("\"{escaped}\"")
}

/// Write `tmux.conf` into `dir`, replacing any previous one, and return its
/// path. The caller picks a per-run or cache directory.
///
/// Rewriting this file does not change a running server. tmux reads `-f` only
/// when it starts one, and every zax session shares the single `-L zax`
/// socket, so the prefix is a property of that shared server: the first
/// session to start it fixes the prefix for every repository until it exits.
/// A later repository's `[tmux] prefix` is written here and has no effect
/// until the server is restarted. That is the accepted cost of one socket,
/// which is what keeps `zax session list` whole and sessions persistent.
pub fn write_to(dir: &Path, prefix: &str, zax: &Path) -> Result<PathBuf, std::io::Error> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join("tmux.conf");
    std::fs::write(&path, render(prefix, zax))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;

    #[test]
    fn the_config_popup_is_bound_and_runs_this_build_of_zax() {
        // `{{ZAX}}` rather than `zax`, so the popup runs the binary the session
        // was started from and not whatever is first on PATH.
        let rendered = render("C-Space", Path::new("/nix/store/abc/bin/zax"));
        assert!(
            rendered.contains("bind C display-popup"),
            "got:\n{rendered}"
        );
        assert!(
            rendered.contains("'/nix/store/abc/bin/zax config'"),
            "got:\n{rendered}"
        );
        assert!(!rendered.contains("{{ZAX}}"), "got:\n{rendered}");
    }

    #[test]
    fn the_popup_starts_in_the_pane_s_own_directory() {
        // `zax config` resolves the repository from its working directory. A
        // popup without this starts wherever the server did, which is another
        // worktree, and would edit that repository's config.
        let rendered = render("C-Space", Path::new("/bin/zax"));
        assert!(
            rendered.contains("-d '#{pane_current_path}'"),
            "got:\n{rendered}"
        );
    }

    #[test]
    fn a_binary_path_that_needs_quoting_gets_it() {
        // The popup command is one shell word per space, inside tmux's own
        // single quotes, so a path with a space has to be quoted for the shell
        // in a way that survives being inside them.
        let rendered = render("C-Space", Path::new("/opt/my tools/zax"));
        assert!(
            rendered.contains(r#"'"/opt/my tools/zax" config'"#),
            "got:\n{rendered}"
        );
    }

    #[test]
    fn the_prefix_is_substituted_everywhere_it_appears() {
        let rendered = render("C-q", Path::new("/bin/zax"));
        assert!(rendered.contains("set -g prefix C-q"));
        assert!(rendered.contains("bind C-q send-prefix"));
        assert!(!rendered.contains("{{PREFIX}}"));
    }

    #[test]
    fn the_status_bar_is_off_because_the_zed_terminal_is_small() {
        assert!(render("C-Space", Path::new("/bin/zax")).contains("set -g status off"));
    }

    #[test]
    fn unprefixed_alt_keys_switch_panes_so_pane_zero_swallows_few_keystrokes() {
        let rendered = render("C-Space", Path::new("/bin/zax"));
        assert!(rendered.contains("bind -n M-h select-pane -L"));
        assert!(rendered.contains("bind -n M-l select-pane -R"));
    }

    #[test]
    fn the_agent_pane_title_is_forwarded_to_the_outer_terminal() {
        // Zed names a terminal thread from the OSC 0/2 title its pty receives,
        // falling back to the foreground process. tmux swallows that sequence
        // unless `set-titles` is on, so Zed sees `tmux` and Claude Code's
        // session name never arrives.
        let rendered = render("C-Space", Path::new("/bin/zax"));
        assert!(rendered.contains("set -g set-titles on"));
    }

    #[test]
    fn the_forwarded_title_is_pinned_to_the_agent_pane_not_the_focused_one() {
        // `#{pane_title}` resolves against whichever pane is active, so moving
        // focus to a context pane renamed the Zed thread after it. Looping the
        // window's panes and emitting only pane 0's title keeps the thread
        // named by Claude no matter where focus sits. tmux's default string is
        // no good either: it prefixes session, window and index, reaching Zed
        // as `zax-repo-branch:0:zsh - "..."`.
        let rendered = render("C-Space", Path::new("/bin/zax"));
        assert!(rendered.contains(
            r##"set -g set-titles-string "#{P:#{?#{==:#{pane_index},0},#{pane_title},}}""##
        ));
    }

    #[test]
    fn the_default_prefix_binding_is_unbound() {
        assert!(render("C-Space", Path::new("/bin/zax")).contains("unbind C-b"));
    }

    #[test]
    fn writing_the_conf_yields_a_readable_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = write_to(tmp.path(), "C-q", Path::new("/bin/zax")).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("C-q"));
        assert_eq!(path.file_name().unwrap(), "tmux.conf");
    }

    #[test]
    fn rewriting_the_conf_replaces_the_previous_one() {
        let tmp = tempfile::TempDir::new().unwrap();
        write_to(tmp.path(), "C-q", Path::new("/bin/zax")).unwrap();
        let path = write_to(tmp.path(), "C-a", Path::new("/bin/zax")).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("set -g prefix C-a"));
        assert!(!text.contains("C-q"));
    }
}
