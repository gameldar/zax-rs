//! The TOML as written, before it becomes domain types.

use serde::Deserialize;

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawConfig {
    /// Overrides the per-repo override directory key, for the rare case of two
    /// repositories with the same directory name.
    pub name: Option<String>,
    pub agent: Option<RawAgent>,
    pub layout: Option<RawLayout>,
    pub worktree: Option<RawWorktree>,
    pub tmux: Option<RawTmux>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawAgent {
    pub command: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawLayout {
    pub panes: Option<Vec<RawPane>>,
}

// `deny_unknown_fields` is not supported on an internally tagged enum, so a typo
// inside a pane table is caught by the `kind` match rather than by serde.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum RawPane {
    Builtin {
        name: String,
        size: Option<String>,
    },
    Exec {
        title: String,
        every: String,
        command: Vec<String>,
        size: Option<String>,
    },
    Raw {
        command: Vec<String>,
        size: Option<String>,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawWorktree {
    pub root: Option<String>,
    pub copy: Option<Vec<String>>,
    pub hooks: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawTmux {
    pub prefix: Option<String>,
}
