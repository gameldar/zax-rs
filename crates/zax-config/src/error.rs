use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ConfigError {
    #[error("could not read {path}: {detail}")]
    Read { path: PathBuf, detail: String },
    #[error("{path} is not valid TOML: {detail}")]
    Parse { path: PathBuf, detail: String },
    #[error("invalid `{field}`: {detail}")]
    Invalid { field: &'static str, detail: String },
}
