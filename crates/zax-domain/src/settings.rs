//! The configuration vocabulary the config modal edits, and the view it reads.
//!
//! The three layers are a precedence order, not a set, and the thing a reader
//! of a config most wants to know is which of them supplied the value in front
//! of them. That question is answered once, here, by a pure function over the
//! per-layer values - never by the editor, and never by a file.

use std::path::PathBuf;

/// The three config layers, in precedence order: your layer wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LayerSlot {
    /// `~/.config/zax/config.toml`
    Global,
    /// `<repo>/.zax.toml`
    Repo,
    /// `~/.config/zax/repos/<key>/config.toml`
    Personal,
}

impl LayerSlot {
    pub const ALL: [LayerSlot; 3] = [LayerSlot::Global, LayerSlot::Repo, LayerSlot::Personal];

    /// Its position in a `[_; 3]` keyed on the layer, which is the shape the
    /// view uses so a layer cannot be missing from a row.
    pub fn index(self) -> usize {
        match self {
            Self::Global => 0,
            Self::Repo => 1,
            Self::Personal => 2,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Repo => "repo",
            Self::Personal => "personal",
        }
    }

    /// The single letter the footer uses, where three layers share a line.
    pub fn initial(self) -> &'static str {
        match self {
            Self::Global => "G",
            Self::Repo => "R",
            Self::Personal => "P",
        }
    }

    /// Cycle forwards, for the key that picks the layer edits land in.
    pub fn next(self) -> Self {
        match self {
            Self::Global => Self::Repo,
            Self::Repo => Self::Personal,
            Self::Personal => Self::Global,
        }
    }

    pub fn previous(self) -> Self {
        match self {
            Self::Global => Self::Personal,
            Self::Repo => Self::Global,
            Self::Personal => Self::Repo,
        }
    }
}

/// Where an effective value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Default,
    Layer(LayerSlot),
}

impl Origin {
    pub fn label(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Layer(slot) => slot.label(),
        }
    }
}

/// Whether a setting holds one value or a list of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingShape {
    Scalar,
    List,
}

/// Every setting the modal can edit.
///
/// Closed on purpose. A new config key makes the matches over it fail to compile
/// until they handle it, and the modal's row list is held to `SettingKey::ALL`
/// by a test, so the modal cannot silently omit a setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SettingKey {
    AgentCommand,
    WorktreeRoot,
    WorktreeCopy,
    WorktreeHooks,
    OpenVia,
    OpenCommand,
    EditorVia,
    EditorCommand,
    TicketsProvider,
    TodoFile,
    TmuxPrefix,
    PrsInclude,
    PrsExclude,
    PrsMaxAge,
    RepoKeyName,
}

impl SettingKey {
    /// Display order: grouped as the TOML is, with the repo key last because it
    /// is the one setting that is about zax's own bookkeeping.
    pub const ALL: [SettingKey; 15] = [
        SettingKey::AgentCommand,
        SettingKey::WorktreeRoot,
        SettingKey::WorktreeCopy,
        SettingKey::WorktreeHooks,
        SettingKey::OpenVia,
        SettingKey::OpenCommand,
        SettingKey::EditorVia,
        SettingKey::EditorCommand,
        SettingKey::TicketsProvider,
        SettingKey::TodoFile,
        SettingKey::TmuxPrefix,
        SettingKey::PrsInclude,
        SettingKey::PrsExclude,
        SettingKey::PrsMaxAge,
        SettingKey::RepoKeyName,
    ];

    /// The TOML table this setting lives in, or `None` for a top-level key.
    pub fn group(self) -> Option<&'static str> {
        match self {
            Self::AgentCommand => Some("agent"),
            Self::WorktreeRoot | Self::WorktreeCopy | Self::WorktreeHooks => Some("worktree"),
            Self::OpenVia | Self::OpenCommand => Some("open"),
            Self::EditorVia | Self::EditorCommand => Some("editor"),
            Self::TicketsProvider => Some("tickets"),
            Self::TodoFile => Some("todo"),
            Self::TmuxPrefix => Some("tmux"),
            Self::PrsInclude | Self::PrsExclude | Self::PrsMaxAge => Some("prs"),
            Self::RepoKeyName => None,
        }
    }

    /// The key within its table.
    pub fn label(self) -> &'static str {
        match self {
            Self::AgentCommand | Self::OpenCommand | Self::EditorCommand => "command",
            Self::OpenVia | Self::EditorVia => "via",
            Self::WorktreeRoot => "root",
            Self::WorktreeCopy => "copy",
            Self::WorktreeHooks => "hooks",
            Self::TicketsProvider => "provider",
            Self::TodoFile => "file",
            Self::TmuxPrefix => "prefix",
            Self::PrsInclude => "include",
            Self::PrsExclude => "exclude",
            Self::PrsMaxAge => "max_age",
            Self::RepoKeyName => "name",
        }
    }

    /// `agent.command`, as an error message or a restart notice names it.
    pub fn full_name(self) -> &'static str {
        match self {
            Self::AgentCommand => "agent.command",
            Self::WorktreeRoot => "worktree.root",
            Self::WorktreeCopy => "worktree.copy",
            Self::WorktreeHooks => "worktree.hooks",
            Self::OpenVia => "open.via",
            Self::OpenCommand => "open.command",
            Self::EditorVia => "editor.via",
            Self::EditorCommand => "editor.command",
            Self::TicketsProvider => "tickets.provider",
            Self::TodoFile => "todo.file",
            Self::TmuxPrefix => "tmux.prefix",
            Self::PrsInclude => "prs.include",
            Self::PrsExclude => "prs.exclude",
            Self::PrsMaxAge => "prs.max_age",
            Self::RepoKeyName => "name",
        }
    }

    /// How a list reads on one line. A command is argv - spaces are what
    /// separate its words - while every other list is a set of entries, which
    /// read as a comma-separated list.
    pub fn list_separator(self) -> &'static str {
        match self {
            Self::AgentCommand
            | Self::OpenCommand
            | Self::EditorCommand
            | Self::TicketsProvider => " ",
            _ => ", ",
        }
    }

    pub fn shape(self) -> SettingShape {
        match self {
            Self::AgentCommand
            | Self::WorktreeCopy
            | Self::WorktreeHooks
            | Self::OpenCommand
            | Self::EditorCommand
            | Self::TicketsProvider
            | Self::PrsInclude
            | Self::PrsExclude => SettingShape::List,
            Self::WorktreeRoot
            | Self::OpenVia
            | Self::EditorVia
            | Self::TodoFile
            | Self::TmuxPrefix
            | Self::PrsMaxAge
            | Self::RepoKeyName => SettingShape::Scalar,
        }
    }

    /// One line, shown when the row is selected.
    pub fn help(self) -> &'static str {
        match self {
            Self::AgentCommand => "the agent and its arguments, run in pane 0",
            Self::WorktreeRoot => "every worktree lives under <root>/<repo>/<branch>",
            Self::WorktreeCopy => "paths copied into a new worktree, relative to the repository",
            Self::WorktreeHooks => "shell commands run in a new worktree, in order",
            Self::OpenVia => "command runs open.command; herdr opens the worktree as a herdr space",
            Self::OpenCommand => "what a worktree opens in, path appended; empty opens nothing",
            Self::EditorVia => "command runs editor.command; herdr runs it in a herdr space",
            Self::EditorCommand => {
                "what a file such as the todo list is edited in; empty edits nothing"
            }
            Self::TicketsProvider => {
                "the ticket provider command, such as zax-jira; empty means none"
            }
            Self::TodoFile => "the markdown file the todo pane reads and writes",
            Self::TmuxPrefix => "the tmux prefix key, such as C-Space or C-a",
            Self::PrsInclude => "only these repositories; empty means every repository",
            Self::PrsExclude => "never these repositories, even when included",
            Self::PrsMaxAge => "hide pull requests older than this: 30s, 5m, 2h, 14d",
            Self::RepoKeyName => "the directory under repos/ this repo's personal layer uses",
        }
    }

    /// What zax does when no layer defines it, rendered for display.
    pub fn default_display(self) -> &'static str {
        match self {
            Self::AgentCommand => "claude",
            Self::WorktreeRoot => "~/src/worktrees",
            Self::WorktreeCopy | Self::WorktreeHooks | Self::PrsExclude => "[]",
            Self::OpenVia | Self::EditorVia => "command",
            Self::OpenCommand => "zed -e",
            Self::EditorCommand => "zed -e",
            Self::TicketsProvider => "[] (none)",
            Self::TodoFile => "~/.local/share/zax/todo.md",
            Self::TmuxPrefix => "C-Space",
            Self::PrsInclude => "[] (every repository)",
            Self::PrsMaxAge => "(no limit)",
            Self::RepoKeyName => "(the repository directory name)",
        }
    }

    /// The layers that may define it.
    ///
    /// `override_key` reads `name` from the repository layer alone, so written
    /// anywhere else it parses and is then ignored - which the modal must not
    /// let you do.
    pub fn allowed_slots(self) -> &'static [LayerSlot] {
        match self {
            Self::RepoKeyName => &[LayerSlot::Repo],
            _ => &LayerSlot::ALL,
        }
    }

    pub fn allows(self, slot: LayerSlot) -> bool {
        self.allowed_slots().contains(&slot)
    }

    /// Whether a running session can pick up a change to this setting.
    ///
    /// The prefix belongs to a tmux server that outlives the command, the agent
    /// command and the layout are decided when a session is built, and `name`
    /// decides which file the personal layer is. Everything else is read fresh
    /// by whatever reads it next.
    pub fn needs_restart(self) -> bool {
        matches!(
            self,
            Self::TmuxPrefix | Self::AgentCommand | Self::RepoKeyName
        )
    }
}

/// A value as a layer holds it.
///
/// Two shapes, carrying structure and never meaning: a duration, a repo pattern
/// and a tmux prefix are all `Scalar`, because the authority on whether `"14d"`
/// is a duration is the config crate's parse chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingValue {
    Scalar(String),
    List(Vec<String>),
}

impl SettingValue {
    /// One line, as a row shows it. An empty list is `[]`, which is a value and
    /// not the absence of one.
    pub fn display(&self) -> String {
        self.display_with(", ")
    }

    /// The same, for a list whose items are not separated by commas.
    pub fn display_with(&self, separator: &str) -> String {
        match self {
            Self::Scalar(text) => text.clone(),
            Self::List(items) if items.is_empty() => "[]".to_owned(),
            Self::List(items) => items.join(separator),
        }
    }
}

/// One setting, across the layers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingRow {
    pub key: SettingKey,
    pub per_layer: [Option<SettingValue>; 3],
    pub effective: Option<SettingValue>,
    pub origin: Origin,
    /// Whether this editing session has changed the setting. Not the same
    /// question as whether a layer is unsaved: a saved change is still a change
    /// the reader of the screen made, and for some settings still one a running
    /// session cannot pick up.
    pub changed: bool,
}

impl SettingRow {
    /// The merge rule, stated once: the highest layer defining the key wins,
    /// and with none of them defining it the built-in default does.
    pub fn new(key: SettingKey, per_layer: [Option<SettingValue>; 3]) -> Self {
        let (effective, origin) = LayerSlot::ALL
            .into_iter()
            .rev()
            .find_map(|slot| {
                per_layer
                    .get(slot.index())
                    .and_then(|v| v.clone())
                    .map(|value| (Some(value), Origin::Layer(slot)))
            })
            .unwrap_or((None, Origin::Default));
        Self {
            key,
            per_layer,
            effective,
            origin,
            changed: false,
        }
    }

    pub fn mark_changed(mut self, changed: bool) -> Self {
        self.changed = changed;
        self
    }

    pub fn at(&self, slot: LayerSlot) -> Option<&SettingValue> {
        self.per_layer.get(slot.index()).and_then(Option::as_ref)
    }

    /// One layer's value as a row shows it, or `(unset)` where it has none.
    pub fn display_at(&self, slot: LayerSlot) -> String {
        match self.at(slot) {
            Some(value) => value.display_with(self.key.list_separator()),
            None => "(unset)".to_owned(),
        }
    }

    /// The effective value as the main screen shows it: the built-in default
    /// when no layer defines the setting, and `(unset)` when a layer explicitly
    /// has nothing to say and there is no default either.
    pub fn display_effective(&self) -> String {
        match &self.effective {
            Some(value) => value.display_with(self.key.list_separator()),
            None => self.key.default_display().to_owned(),
        }
    }
}

/// Which of the three pane kinds a pane is aiming at.
///
/// Not `PaneKind`, which carries each kind's validated payload. The editor has
/// to know which kind a half-filled pane is while its fields are still strings,
/// and `PaneKind` by design cannot hold that state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneKindTag {
    Builtin,
    Exec,
    Raw,
}

impl PaneKindTag {
    pub const ALL: [PaneKindTag; 3] = [PaneKindTag::Builtin, PaneKindTag::Exec, PaneKindTag::Raw];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Builtin => "builtin",
            Self::Exec => "exec",
            Self::Raw => "raw",
        }
    }

    /// Which of a pane's fields this kind carries. Everything else must be
    /// absent, which is what `pane_from_raw` enforces and what switching kind
    /// has to drop.
    pub fn fields(self) -> &'static [PaneField] {
        match self {
            Self::Builtin => &[PaneField::Name, PaneField::Size],
            Self::Exec => &[
                PaneField::Title,
                PaneField::Every,
                PaneField::Command,
                PaneField::Size,
            ],
            Self::Raw => &[PaneField::Command, PaneField::Size],
        }
    }

    pub fn carries(self, field: PaneField) -> bool {
        self.fields().contains(&field)
    }
}

/// A pane's editable fields. `size` is common to every kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneField {
    Name,
    Title,
    Every,
    Command,
    Size,
}

impl PaneField {
    pub fn label(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Title => "title",
            Self::Every => "every",
            Self::Command => "command",
            Self::Size => "size",
        }
    }
}

/// A pane as it is being edited: every field optional, mirroring `RawPane`.
///
/// A pane being edited is allowed to be incomplete; a pane being saved is not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneEdit {
    pub kind: PaneKindTag,
    pub name: Option<String>,
    pub title: Option<String>,
    pub every: Option<String>,
    pub command: Option<Vec<String>>,
    pub size: Option<String>,
}

impl PaneEdit {
    /// An empty pane of a kind, as the kind picker produces one.
    pub fn new(kind: PaneKindTag) -> Self {
        Self {
            kind,
            name: None,
            title: None,
            every: None,
            command: None,
            size: None,
        }
    }

    /// Drop whatever the new kind must not carry. Switching kind is the one
    /// edit that destroys fields, so it is a function with a name rather than
    /// an assignment at a call site.
    pub fn change_kind(&self, kind: PaneKindTag) -> Self {
        let keep = |field: PaneField, value: &Option<String>| {
            if kind.carries(field) {
                value.clone()
            } else {
                None
            }
        };
        Self {
            kind,
            name: keep(PaneField::Name, &self.name),
            title: keep(PaneField::Title, &self.title),
            every: keep(PaneField::Every, &self.every),
            command: if kind.carries(PaneField::Command) {
                self.command.clone()
            } else {
                None
            },
            size: keep(PaneField::Size, &self.size),
        }
    }

    /// The fields `change_kind` would drop, so the editor can say what it is
    /// about to throw away before it does.
    pub fn fields_lost_changing_to(&self, kind: PaneKindTag) -> Vec<PaneField> {
        self.kind
            .fields()
            .iter()
            .copied()
            .filter(|field| !kind.carries(*field) && self.field(*field).is_some())
            .collect()
    }

    pub fn field(&self, field: PaneField) -> Option<String> {
        match field {
            PaneField::Name => self.name.clone(),
            PaneField::Title => self.title.clone(),
            PaneField::Every => self.every.clone(),
            PaneField::Command => self.command.as_ref().map(|argv| argv.join(" ")),
            PaneField::Size => self.size.clone(),
        }
    }
}

/// The pane list, which belongs entirely to one layer: arrays replace
/// wholesale, so there is no such thing as a pane list assembled from two.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanesView {
    pub panes: Vec<PaneEdit>,
    pub origin: Origin,
    pub changed: bool,
}

/// A change a save has written to a file but a running session cannot pick up.
///
/// Only the settings that are decided once: the prefix belongs to a tmux server
/// that outlives the command, the agent command and the layout are read when a
/// session is built, and `name` decides which file the personal layer is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pending {
    Setting(SettingKey),
    Panes,
}

/// What a save did: the files it wrote, and what is still pending.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Saved {
    pub written: Vec<LayerSlot>,
    pub pending: Vec<Pending>,
}

/// One layer's file, as the footer and the save report talk about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerInfo {
    pub slot: LayerSlot,
    pub path: PathBuf,
    pub exists: bool,
    pub dirty: bool,
}

/// Everything the modal draws, and the only thing it is given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigView {
    pub settings: Vec<SettingRow>,
    pub panes: PanesView,
    pub layers: [LayerInfo; 3],
    /// A rendered message rather than a `ConfigError`: that type lives in
    /// `zax-config`, which the domain may not depend on, and the modal only
    /// ever displays it.
    pub status: Result<(), String>,
}

impl ConfigView {
    pub fn row(&self, key: SettingKey) -> Option<&SettingRow> {
        self.settings.iter().find(|row| row.key == key)
    }

    pub fn layer(&self, slot: LayerSlot) -> Option<&LayerInfo> {
        self.layers.iter().find(|layer| layer.slot == slot)
    }

    pub fn any_dirty(&self) -> bool {
        self.layers.iter().any(|layer| layer.dirty)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;

    fn scalar(text: &str) -> Option<SettingValue> {
        Some(SettingValue::Scalar(text.to_owned()))
    }

    fn list(items: &[&str]) -> Option<SettingValue> {
        Some(SettingValue::List(
            items.iter().map(|s| (*s).to_owned()).collect(),
        ))
    }

    #[test]
    fn the_editor_command_is_an_argv_list_shown_the_way_a_command_line_reads() {
        // Like `agent.command` and unlike every other list, this is one
        // command rather than a set of entries, so it reads with spaces.
        assert_eq!(SettingKey::EditorCommand.shape(), SettingShape::List);
        assert_eq!(SettingKey::EditorCommand.list_separator(), " ");
        let row = SettingRow::new(SettingKey::EditorCommand, [None, None, None]);
        assert_eq!(row.display_effective(), "zed -e");
    }

    #[test]
    fn the_open_command_is_a_command_line_in_its_own_table() {
        assert_eq!(SettingKey::OpenCommand.full_name(), "open.command");
        assert_eq!(SettingKey::OpenCommand.group(), Some("open"));
        assert_eq!(SettingKey::OpenCommand.list_separator(), " ");
        let row = SettingRow::new(SettingKey::OpenCommand, [None, None, None]);
        assert_eq!(row.display_effective(), "zed -e");
    }

    #[test]
    fn each_via_is_a_scalar_that_defaults_to_running_a_command() {
        for (key, name) in [
            (SettingKey::OpenVia, "open.via"),
            (SettingKey::EditorVia, "editor.via"),
        ] {
            assert_eq!(key.full_name(), name);
            assert_eq!(key.label(), "via");
            assert_eq!(key.shape(), SettingShape::Scalar);
            assert!(key.help().contains("herdr"), "{}", key.help());
            let row = SettingRow::new(key, [None, None, None]);
            assert_eq!(row.display_effective(), "command");
        }
    }

    #[test]
    fn each_table_lists_its_via_before_its_command() {
        let at = |key| SettingKey::ALL.iter().position(|k| *k == key).unwrap();
        assert!(at(SettingKey::OpenVia) + 1 == at(SettingKey::OpenCommand));
        assert!(at(SettingKey::EditorVia) + 1 == at(SettingKey::EditorCommand));
    }

    #[test]
    fn the_todo_file_is_a_scalar_with_a_documented_default() {
        assert_eq!(SettingKey::TodoFile.full_name(), "todo.file");
        assert_eq!(SettingKey::TodoFile.shape(), SettingShape::Scalar);
        let row = SettingRow::new(SettingKey::TodoFile, [None, None, None]);
        assert_eq!(row.display_effective(), "~/.local/share/zax/todo.md");
    }

    #[test]
    fn the_tickets_provider_reads_as_a_command_line() {
        assert_eq!(SettingKey::TicketsProvider.list_separator(), " ");
        assert_eq!(SettingKey::TicketsProvider.full_name(), "tickets.provider");
        let row = SettingRow::new(SettingKey::TicketsProvider, [None, None, None]);
        assert_eq!(row.display_effective(), "[] (none)");
    }

    #[test]
    fn an_empty_editor_command_reads_as_the_setting_that_opens_nothing() {
        let row = SettingRow::new(SettingKey::EditorCommand, [None, None, list(&[])]);
        assert_eq!(row.display_effective(), "[]");
    }

    #[test]
    fn the_highest_layer_defining_a_setting_supplies_the_effective_value() {
        let row = SettingRow::new(
            SettingKey::TmuxPrefix,
            [scalar("C-Space"), scalar("C-a"), scalar("C-q")],
        );
        assert_eq!(row.effective, scalar("C-q"));
        assert_eq!(row.origin, Origin::Layer(LayerSlot::Personal));
    }

    #[test]
    fn a_gap_in_the_top_layer_falls_through_to_the_one_below() {
        let row = SettingRow::new(
            SettingKey::TmuxPrefix,
            [scalar("C-Space"), scalar("C-a"), None],
        );
        assert_eq!(row.effective, scalar("C-a"));
        assert_eq!(row.origin, Origin::Layer(LayerSlot::Repo));
    }

    #[test]
    fn a_setting_no_layer_defines_comes_from_the_built_in_default() {
        let row = SettingRow::new(SettingKey::TmuxPrefix, [None, None, None]);
        assert_eq!(row.effective, None);
        assert_eq!(row.origin, Origin::Default);
        // The row still has something to show: the default zax would use.
        assert_eq!(row.display_effective(), "C-Space");
    }

    #[test]
    fn an_explicitly_empty_list_is_a_value_and_not_the_absence_of_one() {
        // `hooks = []` clears an inherited list; an absent `hooks` inherits it.
        // The two have to resolve differently or the modal cannot express the
        // difference the config semantics already have.
        let cleared = SettingRow::new(
            SettingKey::WorktreeHooks,
            [list(&["direnv allow"]), None, list(&[])],
        );
        let inherited = SettingRow::new(
            SettingKey::WorktreeHooks,
            [list(&["direnv allow"]), None, None],
        );

        assert_eq!(cleared.effective, list(&[]));
        assert_eq!(cleared.origin, Origin::Layer(LayerSlot::Personal));
        assert_eq!(inherited.effective, list(&["direnv allow"]));
        assert_eq!(inherited.origin, Origin::Layer(LayerSlot::Global));
        assert_ne!(cleared.display_effective(), inherited.display_effective());
        assert_eq!(cleared.display_effective(), "[]");
    }

    #[test]
    fn a_row_can_be_asked_for_one_layers_value_without_indexing() {
        let row = SettingRow::new(
            SettingKey::TmuxPrefix,
            [scalar("C-Space"), None, scalar("C-q")],
        );
        assert_eq!(row.at(LayerSlot::Global), scalar("C-Space").as_ref());
        assert_eq!(row.at(LayerSlot::Repo), None);
        assert_eq!(row.at(LayerSlot::Personal), scalar("C-q").as_ref());
    }

    #[test]
    fn the_repo_key_name_may_only_be_written_in_the_repository_layer() {
        // Written anywhere else it parses and is then ignored, because
        // `override_key` reads `name` from the repo layer alone.
        assert_eq!(
            SettingKey::RepoKeyName.allowed_slots(),
            [LayerSlot::Repo].as_slice()
        );
        assert!(SettingKey::RepoKeyName.allows(LayerSlot::Repo));
        assert!(!SettingKey::RepoKeyName.allows(LayerSlot::Personal));
        assert!(!SettingKey::RepoKeyName.allows(LayerSlot::Global));
    }

    #[test]
    fn every_other_setting_may_be_written_in_any_layer() {
        for key in SettingKey::ALL {
            if key == SettingKey::RepoKeyName {
                continue;
            }
            for slot in LayerSlot::ALL {
                assert!(key.allows(slot), "{} in {}", key.full_name(), slot.label());
            }
        }
    }

    #[test]
    fn every_setting_names_itself_by_its_table_and_key() {
        assert_eq!(SettingKey::AgentCommand.group(), Some("agent"));
        assert_eq!(SettingKey::AgentCommand.label(), "command");
        assert_eq!(SettingKey::AgentCommand.full_name(), "agent.command");
        // The override key is a top-level key, not a table entry.
        assert_eq!(SettingKey::RepoKeyName.group(), None);
        assert_eq!(SettingKey::RepoKeyName.full_name(), "name");
    }

    #[test]
    fn the_list_settings_are_the_ones_whose_toml_is_an_array() {
        let lists: Vec<SettingKey> = SettingKey::ALL
            .into_iter()
            .filter(|k| k.shape() == SettingShape::List)
            .collect();
        assert_eq!(
            lists,
            vec![
                SettingKey::AgentCommand,
                SettingKey::WorktreeCopy,
                SettingKey::WorktreeHooks,
                SettingKey::OpenCommand,
                SettingKey::EditorCommand,
                SettingKey::TicketsProvider,
                SettingKey::PrsInclude,
                SettingKey::PrsExclude,
            ]
        );
    }

    #[test]
    fn the_layers_are_in_precedence_order_and_cycle() {
        assert!(LayerSlot::Global < LayerSlot::Repo);
        assert!(LayerSlot::Repo < LayerSlot::Personal);
        assert_eq!(LayerSlot::Global.index(), 0);
        assert_eq!(LayerSlot::Personal.index(), 2);
        assert_eq!(LayerSlot::Personal.next(), LayerSlot::Global);
        assert_eq!(LayerSlot::Global.previous(), LayerSlot::Personal);
    }

    #[test]
    fn an_origin_names_the_layer_or_the_default() {
        assert_eq!(Origin::Default.label(), "default");
        assert_eq!(Origin::Layer(LayerSlot::Repo).label(), "repo");
        assert_eq!(Origin::Layer(LayerSlot::Personal).label(), "personal");
    }

    #[test]
    fn a_list_value_reads_as_its_items_and_an_empty_one_as_brackets() {
        assert_eq!(
            SettingValue::List(vec!["acme/*".to_owned(), "gameldar/zax".to_owned()]).display(),
            "acme/*, gameldar/zax"
        );
        assert_eq!(SettingValue::List(Vec::new()).display(), "[]");
        assert_eq!(SettingValue::Scalar("14d".to_owned()).display(), "14d");
    }

    #[test]
    fn each_pane_kind_carries_exactly_the_fields_its_parser_requires() {
        // These field sets are `pane_from_raw`'s require/reject rules, which is
        // what decides whether a saved pane list starts a session.
        assert_eq!(
            PaneKindTag::Builtin.fields(),
            [PaneField::Name, PaneField::Size].as_slice()
        );
        assert_eq!(
            PaneKindTag::Exec.fields(),
            [
                PaneField::Title,
                PaneField::Every,
                PaneField::Command,
                PaneField::Size
            ]
            .as_slice()
        );
        assert_eq!(
            PaneKindTag::Raw.fields(),
            [PaneField::Command, PaneField::Size].as_slice()
        );
        assert!(PaneKindTag::Exec.carries(PaneField::Every));
        assert!(!PaneKindTag::Builtin.carries(PaneField::Every));
    }

    #[test]
    fn a_new_pane_of_a_kind_starts_with_every_field_empty() {
        let pane = PaneEdit::new(PaneKindTag::Exec);
        assert_eq!(pane.kind, PaneKindTag::Exec);
        assert_eq!(pane.title, None);
        assert_eq!(pane.command, None);
        assert_eq!(pane.size, None);
    }

    #[test]
    fn changing_a_panes_kind_drops_the_fields_the_new_kind_must_not_carry() {
        // An `exec` pane's title and interval are exactly what a `raw` pane is
        // forbidden to have, so keeping them would produce a layout that parses
        // as an error at the next session start.
        let exec = PaneEdit {
            kind: PaneKindTag::Exec,
            name: None,
            title: Some("Deploys".to_owned()),
            every: Some("60s".to_owned()),
            command: Some(vec!["kubectl".to_owned()]),
            size: Some("30%".to_owned()),
        };

        let lost = exec.fields_lost_changing_to(PaneKindTag::Raw);
        assert_eq!(lost, vec![PaneField::Title, PaneField::Every]);

        let as_raw = exec.change_kind(PaneKindTag::Raw);
        assert_eq!(as_raw.kind, PaneKindTag::Raw);
        assert_eq!(as_raw.title, None);
        assert_eq!(as_raw.every, None);
        // The fields the new kind does carry survive, so a kind picked by
        // mistake is not a retyped command.
        assert_eq!(as_raw.command, Some(vec!["kubectl".to_owned()]));
        assert_eq!(as_raw.size, Some("30%".to_owned()));
    }

    #[test]
    fn changing_to_the_same_kind_loses_nothing() {
        let pane = PaneEdit {
            kind: PaneKindTag::Builtin,
            name: Some("prs-for-review".to_owned()),
            title: None,
            every: None,
            command: None,
            size: Some("30%".to_owned()),
        };
        assert!(pane
            .fields_lost_changing_to(PaneKindTag::Builtin)
            .is_empty());
        assert_eq!(pane.change_kind(PaneKindTag::Builtin), pane);
    }

    #[test]
    fn a_view_answers_for_a_row_and_a_layer_by_name() {
        let view = ConfigView {
            settings: vec![SettingRow::new(
                SettingKey::TmuxPrefix,
                [None, None, scalar("C-q")],
            )],
            panes: PanesView {
                panes: Vec::new(),
                origin: Origin::Default,
                changed: false,
            },
            layers: [
                LayerInfo {
                    slot: LayerSlot::Global,
                    path: PathBuf::from("/g"),
                    exists: true,
                    dirty: false,
                },
                LayerInfo {
                    slot: LayerSlot::Repo,
                    path: PathBuf::from("/r"),
                    exists: false,
                    dirty: false,
                },
                LayerInfo {
                    slot: LayerSlot::Personal,
                    path: PathBuf::from("/p"),
                    exists: true,
                    dirty: true,
                },
            ],
            status: Ok(()),
        };

        assert_eq!(
            view.row(SettingKey::TmuxPrefix).map(|r| r.origin),
            Some(Origin::Layer(LayerSlot::Personal))
        );
        assert_eq!(view.row(SettingKey::PrsMaxAge), None);
        assert_eq!(
            view.layer(LayerSlot::Repo).map(|l| l.path.clone()),
            Some(PathBuf::from("/r"))
        );
        assert!(view.any_dirty());
    }

    #[test]
    fn the_settings_a_running_session_cannot_pick_up_are_the_ones_a_restart_notice_names() {
        for key in [
            SettingKey::TmuxPrefix,
            SettingKey::AgentCommand,
            SettingKey::RepoKeyName,
        ] {
            assert!(key.needs_restart(), "{}", key.full_name());
        }
        for key in [
            SettingKey::WorktreeRoot,
            SettingKey::WorktreeCopy,
            SettingKey::WorktreeHooks,
            SettingKey::PrsInclude,
            SettingKey::PrsExclude,
            SettingKey::PrsMaxAge,
        ] {
            assert!(!key.needs_restart(), "{}", key.full_name());
        }
    }
}
