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
    pub prs: Option<RawPrs>,
    pub editor: Option<RawEditor>,
    pub tickets: Option<RawTickets>,
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

/// One `[[layout.panes]]` table, flat rather than an enum.
///
/// The obvious shape here is an internally tagged enum keyed on `kind`, and it
/// was the first attempt. serde cannot apply `deny_unknown_fields` to one, and
/// the consequence is not academic: `siz = "30%"` parsed happily and the pane
/// silently took a default size, in the one table people hand-edit most. Every
/// other typo in this file is an error naming the key, so that gap would have
/// been the single place a mistyped setting vanished without a word.
///
/// Flat, every field optional, `deny_unknown_fields` on. Which fields a given
/// `kind` actually requires, and which it must not carry, is then checked in
/// `parse::pane_from_raw`, where the error can say what was wrong.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawPane {
    pub kind: String,
    pub name: Option<String>,
    pub title: Option<String>,
    pub every: Option<String>,
    pub command: Option<Vec<String>>,
    pub size: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawWorktree {
    pub root: Option<String>,
    pub copy: Option<Vec<String>>,
    pub hooks: Option<Vec<String>>,
}

/// The `[editor]` table: what a freshly made worktree is opened in.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawEditor {
    pub command: Option<Vec<String>>,
}

/// The `[tickets]` table: which ticket provider the tickets pane runs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawTickets {
    pub provider: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawTmux {
    pub prefix: Option<String>,
}

/// The `[prs]` table: which pull requests the review-requests pane shows.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawPrs {
    pub include: Option<Vec<String>>,
    pub exclude: Option<Vec<String>>,
    pub max_age: Option<String>,
}
