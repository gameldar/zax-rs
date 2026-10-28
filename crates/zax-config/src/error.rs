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
    #[error("{path}: invalid `{field}`: {detail}")]
    InvalidLayer {
        path: PathBuf,
        field: &'static str,
        detail: String,
    },
    #[error("invalid `keymap.{table}.\"{sequence}\"`: {detail}")]
    Keymap {
        table: String,
        sequence: String,
        detail: String,
    },
    #[error("`keymap.{0}` is not a keymap table; write all, {tables}", tables = zax_domain::PaneContext::ALL.map(|c| c.as_str()).join(", "))]
    UnknownKeymapTable(String),
    #[error("`{0}` is not a pane kind; write builtin, exec or raw")]
    UnknownPaneKind(String),
    #[error("a `{kind}` pane needs a `{field}`")]
    MissingPaneField {
        kind: &'static str,
        field: &'static str,
    },
    #[error("a `{kind}` pane has no `{field}`")]
    UnexpectedPaneField {
        kind: &'static str,
        field: &'static str,
    },
}
