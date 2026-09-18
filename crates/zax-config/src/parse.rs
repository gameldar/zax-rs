//! Turning the merged TOML into domain types. Validation happens here, once, at
//! the boundary, so nothing downstream re-checks it.

use std::path::PathBuf;

use zax_domain::{
    parse_duration, BuiltinPane, Command, LayoutSpec, PaneKind, PaneSize, PaneSpec, WorktreeRoot,
};

use crate::error::ConfigError;
use crate::raw::{RawConfig, RawPane};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub layout: LayoutSpec,
    pub worktree: WorktreeConfig,
    pub tmux: TmuxConfig,
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
        .map(PathBuf::from)
        .collect();

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

    Ok(Config {
        layout,
        worktree: WorktreeConfig { root, copy, hooks },
        tmux: TmuxConfig { prefix },
    })
}

/// With no configured layout, both built-ins are shown.
fn default_panes() -> Result<Vec<PaneSpec>, ConfigError> {
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

fn pane_from_raw(raw: RawPane) -> Result<PaneSpec, ConfigError> {
    match raw {
        RawPane::Builtin { name, size } => Ok(PaneSpec {
            kind: PaneKind::Builtin(BuiltinPane::parse(&name).map_err(|e| {
                ConfigError::Invalid {
                    field: "layout.panes.name",
                    detail: e.to_string(),
                }
            })?),
            size: parse_size(size)?,
        }),
        RawPane::Exec {
            title,
            every,
            command,
            size,
        } => Ok(PaneSpec {
            kind: PaneKind::Exec {
                title,
                every: parse_duration(&every).map_err(|e| ConfigError::Invalid {
                    field: "layout.panes.every",
                    detail: e.to_string(),
                })?,
                command: Command::new(command).map_err(|e| ConfigError::Invalid {
                    field: "layout.panes.command",
                    detail: e.to_string(),
                })?,
            },
            size: parse_size(size)?,
        }),
        RawPane::Raw { command, size } => Ok(PaneSpec {
            kind: PaneKind::Raw {
                command: Command::new(command).map_err(|e| ConfigError::Invalid {
                    field: "layout.panes.command",
                    detail: e.to_string(),
                })?,
            },
            size: parse_size(size)?,
        }),
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
    fn a_relative_worktree_root_is_rejected() {
        let raw: RawConfig = toml::from_str("[worktree]\nroot = \"worktrees\"").unwrap();
        assert!(from_raw(raw, "/home/me").is_err());
    }
}
