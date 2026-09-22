//! Rendering and writing zax's own tmux configuration.

use std::path::{Path, PathBuf};

pub const DEFAULT_CONF_TEMPLATE: &str = include_str!("../assets/tmux.conf");

/// Substitute the configured prefix key into the template.
pub fn render(prefix: &str) -> String {
    DEFAULT_CONF_TEMPLATE.replace("{{PREFIX}}", prefix)
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
pub fn write_to(dir: &Path, prefix: &str) -> Result<PathBuf, std::io::Error> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join("tmux.conf");
    std::fs::write(&path, render(prefix))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;

    #[test]
    fn the_prefix_is_substituted_everywhere_it_appears() {
        let rendered = render("C-q");
        assert!(rendered.contains("set -g prefix C-q"));
        assert!(rendered.contains("bind C-q send-prefix"));
        assert!(!rendered.contains("{{PREFIX}}"));
    }

    #[test]
    fn the_status_bar_is_off_because_the_zed_terminal_is_small() {
        assert!(render("C-Space").contains("set -g status off"));
    }

    #[test]
    fn unprefixed_alt_keys_switch_panes_so_pane_zero_swallows_few_keystrokes() {
        let rendered = render("C-Space");
        assert!(rendered.contains("bind -n M-h select-pane -L"));
        assert!(rendered.contains("bind -n M-l select-pane -R"));
    }

    #[test]
    fn the_agent_pane_title_is_forwarded_to_the_outer_terminal() {
        // Zed names a terminal thread from the OSC 0/2 title its pty receives,
        // falling back to the foreground process. tmux swallows that sequence
        // unless `set-titles` is on, so Zed sees `tmux` and Claude Code's
        // session name never arrives.
        let rendered = render("C-Space");
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
        let rendered = render("C-Space");
        assert!(rendered.contains(
            r##"set -g set-titles-string "#{P:#{?#{==:#{pane_index},0},#{pane_title},}}""##
        ));
    }

    #[test]
    fn the_default_prefix_binding_is_unbound() {
        assert!(render("C-Space").contains("unbind C-b"));
    }

    #[test]
    fn writing_the_conf_yields_a_readable_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = write_to(tmp.path(), "C-q").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("C-q"));
        assert_eq!(path.file_name().unwrap(), "tmux.conf");
    }

    #[test]
    fn rewriting_the_conf_replaces_the_previous_one() {
        let tmp = tempfile::TempDir::new().unwrap();
        write_to(tmp.path(), "C-q").unwrap();
        let path = write_to(tmp.path(), "C-a").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("set -g prefix C-a"));
        assert!(!text.contains("C-q"));
    }
}
