//! Rendering and writing zax's own tmux configuration.

use std::path::{Path, PathBuf};

pub const DEFAULT_CONF_TEMPLATE: &str = include_str!("../assets/tmux.conf");

/// Substitute the configured prefix key into the template.
pub fn render(prefix: &str) -> String {
    DEFAULT_CONF_TEMPLATE.replace("{{PREFIX}}", prefix)
}

/// Write `tmux.conf` into `dir`, replacing any previous one, and return its
/// path. The caller picks a per-run or cache directory.
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
