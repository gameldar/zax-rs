//! The three-layer TOML chain: global defaults, what the repo ships, and your
//! personal override for that repo. Your layer wins.

pub mod error;
pub mod merge;
pub mod parse;
pub mod raw;

use std::path::{Path, PathBuf};

pub use error::ConfigError;
pub use parse::{Config, TmuxConfig, WorktreeConfig};
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
    toml::from_str(&text).map_err(|e| ConfigError::Parse {
        path: path.to_owned(),
        detail: e.to_string(),
    })
}
