//! The three-layer TOML chain: global defaults, what the repo ships, and your
//! personal override for that repo. Your layer wins.
//!
//! Layer 2 is a trust boundary. It is a file checked into somebody else's
//! repository, and zax runs what it says: pane commands are executed when a
//! session starts, `[worktree] hooks` are executed through `sh -c` when a
//! worktree is made, and a `[tickets] provider` is started by the shared
//! per-user broker, which runs it outside the repository's session and keeps
//! it running while any pane subscribes to it. That is the intended design
//! of a layer whose purpose is shipping a repo's own layout, but it means
//! opening an untrusted repository with zax as the agent entry point runs
//! that repository's configuration. The values that are not already "the
//! repo can run commands" are parsed strictly here for that reason: the
//! override `name`, the tmux prefix and the `copy` list.

pub mod draft;
pub mod error;
pub mod merge;
pub mod parse;
pub mod raw;

use std::path::{Path, PathBuf};

pub use draft::{Draft, SaveError};
pub use error::ConfigError;
pub use parse::{
    Config, EditorConfig, OpenConfig, TicketsConfig, TmuxConfig, TodoConfig, WorktreeConfig,
};
use raw::RawConfig;
use zax_domain::RootRepoName;

/// `~/.config/zax/config.toml`
pub fn global_config_path(config_home: &Path) -> PathBuf {
    config_home.join("zax").join("config.toml")
}

/// `<repo>/.zax.toml`
pub fn repo_config_path(main_worktree: &Path) -> PathBuf {
    main_worktree.join(".zax.toml")
}

/// `~/.config/zax/repos/<key>/config.toml`
pub fn override_config_path(config_home: &Path, key: &str) -> PathBuf {
    config_home
        .join("zax")
        .join("repos")
        .join(key)
        .join("config.toml")
}

/// Repo overrides are keyed on the root repo directory name, the same value the
/// worktree path rule derives, so it needs no network. A `name` key in the
/// repo's own config wins, for the rare collision.
pub fn override_key(repo: &RootRepoName, repo_layer: &RawConfig) -> String {
    repo_layer
        .name
        .clone()
        .unwrap_or_else(|| repo.as_str().to_owned())
}

/// Load and merge all three layers. A layer whose file is absent is an empty
/// layer, not an error.
pub fn load(
    config_home: &Path,
    main_worktree: &Path,
    repo: &RootRepoName,
    home: &str,
) -> Result<Config, ConfigError> {
    let global = read_layer(&global_config_path(config_home))?;
    let repo_layer = read_layer(&repo_config_path(main_worktree))?;
    let key = override_key(repo, &repo_layer);
    let personal = read_layer(&override_config_path(config_home, &key))?;

    parse::from_raw(merge::merge(vec![global, repo_layer, personal]), home)
}

/// The override key names a directory under `~/.config/zax/repos/`, so it must
/// be one directory name and nothing else.
///
/// This is the only value a repository's own checked-in config contributes to a
/// decision about which file to read, and the file it decides on is the layer
/// that is supposed to beat it. Unchecked, `name = "../../elsewhere"` reads a
/// file the repo can ship itself, `name = "acme"` reads a different repo's
/// personal override, and any name whose directory does not exist silently
/// suppresses yours. A repo can suggest a layout; it can never impose one.
pub(crate) fn validate_override_name(name: &str) -> Result<(), String> {
    // Spelled out rather than run through `Path::components`, which normalises
    // a trailing separator away and would accept `acme/` as one component. A
    // backslash is not a separator here, but a checked-in config travels.
    let acceptable = !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains('\0');
    if acceptable {
        return Ok(());
    }
    Err(format!(
        "`{name}` is not a directory name; it must be a single component, \
         with no separator, no `.` or `..`, and not an absolute path"
    ))
}

fn read_layer(path: &Path) -> Result<RawConfig, ConfigError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(RawConfig::default()),
        Err(e) => {
            return Err(ConfigError::Read {
                path: path.to_owned(),
                detail: e.to_string(),
            })
        }
    };
    parse_layer_text(path, &text)
}

/// One layer's text, as the three-layer chain takes it.
///
/// Separate from reading the file so the config modal can run its own edited
/// text through the identical step. A second implementation of this is a second
/// opinion about what a layer may say, which is exactly what must not exist:
/// the modal's job is to refuse what a session start would refuse.
pub(crate) fn parse_layer_text(path: &Path, text: &str) -> Result<RawConfig, ConfigError> {
    let raw: RawConfig = toml::from_str(text).map_err(|e| ConfigError::Parse {
        path: path.to_owned(),
        detail: e.to_string(),
    })?;

    if let Some(name) = &raw.name {
        validate_override_name(name).map_err(|detail| ConfigError::InvalidLayer {
            path: path.to_owned(),
            field: "name",
            detail,
        })?;
    }
    Ok(raw)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;

    #[test]
    fn an_override_name_is_one_directory_name() {
        for good in ["acme", "acme-backend", "omega.v2", "a b"] {
            assert!(validate_override_name(good).is_ok(), "`{good}` is a name");
        }
    }

    #[test]
    fn an_override_name_may_not_leave_the_repos_directory() {
        for bad in [
            "",
            ".",
            "..",
            "../acme",
            "a/b",
            "/etc/zax",
            "acme/",
            "./acme",
            "back\\slash",
        ] {
            assert!(
                validate_override_name(bad).is_err(),
                "`{bad}` is not a directory name"
            );
        }
    }
}
