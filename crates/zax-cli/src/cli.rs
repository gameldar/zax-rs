//! The command surface.

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "zax",
    version,
    about = "Zed Agent eXtensions: the agent terminal, with context around it"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// The Zed entry point, plus escape hatches.
    Session {
        #[command(subcommand)]
        command: SessionCommand,
    },
    /// Worktree utilities that enforce the project's naming and path rules.
    Worktree {
        #[command(subcommand)]
        command: WorktreeCommand,
    },
    /// Pane processes. Normally launched by `zax session start`, not by hand.
    Pane {
        #[command(subcommand)]
        command: PaneCommand,
    },
    /// View and edit the three config layers.
    Config {
        #[command(subcommand)]
        command: Option<ConfigCommand>,
    },
    /// Verify that tmux, git, gh and the editor are present and authenticated.
    Doctor,
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Print every setting, its effective value and the layer it came from.
    Show,
}

#[derive(Debug, Subcommand)]
pub enum SessionCommand {
    /// Reattach to a detached session in this worktree, or build a new one
    /// when every session is already attached.
    Start,
    /// List the zax sessions on the dedicated socket.
    List,
    /// Kill a session. Defaults to this worktree's own, when it has only one.
    Kill {
        /// The session name, as `zax session list` prints it.
        name: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum WorktreeCommand {
    /// Create a branch and a worktree at the canonical path, then copy and hooks.
    New {
        /// `<TICKET>-<short-desc>`, or a bare lowercase topic.
        branch: String,
        /// What to branch from. Defaults to the current HEAD.
        #[arg(long)]
        from: Option<String>,
        /// Create the worktree but do not run the repo's setup hooks.
        #[arg(long)]
        no_hooks: bool,
        /// Create the worktree but do not open it in the editor.
        #[arg(long)]
        no_open: bool,
    },
    /// Put a pull request's head branch in a worktree, then copy and hooks.
    Review {
        /// A pull request number or a github.com pull URL.
        pr: String,
        #[arg(long)]
        no_hooks: bool,
        /// Move an existing local branch of that name onto the pull request
        /// head even when that discards commits only the branch has.
        #[arg(long)]
        force: bool,
        /// Create the worktree but do not open it in the editor.
        #[arg(long)]
        no_open: bool,
    },
    /// List the worktrees zax manages for this repository.
    List,
    /// Remove a worktree.
    Rm {
        branch: String,
        /// Remove it even with uncommitted changes.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum PaneCommand {
    /// Pull requests awaiting your review.
    PrsForReview,
    /// The status of this worktree's own pull request.
    BranchStatus,
    /// Your own open pull requests, across every repository.
    MyPrs,
    /// Wrap any command's stdout in a header, refresh loop and scrollback.
    Exec {
        #[arg(long)]
        title: String,
        /// How often to re-run the command: 30s, 5m, 2h.
        #[arg(long, default_value = "60s")]
        every: String,
        /// Everything after `--`.
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn the_command_tree_is_internally_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn session_start_is_the_zed_entry_point() {
        let cli = Cli::try_parse_from(["zax", "session", "start"]).unwrap();
        assert!(matches!(
            cli.command,
            Commands::Session {
                command: SessionCommand::Start
            }
        ));
    }

    #[test]
    fn session_has_list_and_kill_escape_hatches() {
        assert!(Cli::try_parse_from(["zax", "session", "list"]).is_ok());
        let cli = Cli::try_parse_from(["zax", "session", "kill", "zax-acme-topic"]).unwrap();
        match cli.command {
            Commands::Session {
                command: SessionCommand::Kill { name },
            } => {
                assert_eq!(name.as_deref(), Some("zax-acme-topic"));
            }
            other => panic!("expected session kill, got {other:?}"),
        }
        // No name means the current worktree's own session.
        assert!(Cli::try_parse_from(["zax", "session", "kill"]).is_ok());
    }

    #[test]
    fn worktree_new_takes_a_branch_a_base_and_a_no_hooks_flag() {
        let cli = Cli::try_parse_from([
            "zax",
            "worktree",
            "new",
            "OMEGA-1963-stamp",
            "--from",
            "origin/main",
            "--no-hooks",
        ])
        .unwrap();
        match cli.command {
            Commands::Worktree {
                command:
                    WorktreeCommand::New {
                        branch,
                        from,
                        no_hooks,
                        no_open,
                    },
            } => {
                assert_eq!(branch, "OMEGA-1963-stamp");
                assert_eq!(from.as_deref(), Some("origin/main"));
                assert!(no_hooks);
                assert!(!no_open, "a new worktree opens in the editor by default");
            }
            other => panic!("expected worktree new, got {other:?}"),
        }
    }

    #[test]
    fn worktree_review_can_be_told_not_to_open_the_editor() {
        let cli = Cli::try_parse_from(["zax", "worktree", "review", "1963", "--no-open"]).unwrap();
        match cli.command {
            Commands::Worktree {
                command: WorktreeCommand::Review { no_open, .. },
            } => assert!(no_open),
            other => panic!("expected worktree review, got {other:?}"),
        }
    }

    #[test]
    fn worktree_review_accepts_a_number_or_a_url() {
        for reference in ["1963", "https://github.com/acme/omega/pull/1963"] {
            let cli = Cli::try_parse_from(["zax", "worktree", "review", reference]).unwrap();
            match cli.command {
                Commands::Worktree {
                    command: WorktreeCommand::Review { pr, .. },
                } => {
                    assert_eq!(pr, reference);
                }
                other => panic!("expected worktree review, got {other:?}"),
            }
        }
    }

    #[test]
    fn worktree_review_can_be_forced() {
        let cli = Cli::try_parse_from(["zax", "worktree", "review", "1963", "--force"]).unwrap();
        match cli.command {
            Commands::Worktree {
                command: WorktreeCommand::Review { force, .. },
            } => assert!(force),
            other => panic!("expected worktree review, got {other:?}"),
        }
        // Destructive by default would defeat the point.
        let plain = Cli::try_parse_from(["zax", "worktree", "review", "1963"]).unwrap();
        match plain.command {
            Commands::Worktree {
                command: WorktreeCommand::Review { force, .. },
            } => assert!(!force),
            other => panic!("expected worktree review, got {other:?}"),
        }
    }

    #[test]
    fn worktree_rm_can_be_forced() {
        let cli =
            Cli::try_parse_from(["zax", "worktree", "rm", "OMEGA-1963-stamp", "--force"]).unwrap();
        match cli.command {
            Commands::Worktree {
                command: WorktreeCommand::Rm { force, .. },
            } => {
                assert!(force);
            }
            other => panic!("expected worktree rm, got {other:?}"),
        }
    }

    #[test]
    fn the_builtin_pane_subcommands_match_the_builtin_pane_names() {
        // Driven from the domain's own list, so a built-in added there
        // without a subcommand here is a failing test rather than a pane
        // that cannot start.
        for builtin in zax_domain::BuiltinPane::ALL {
            let name = builtin.as_str();
            assert!(
                Cli::try_parse_from(["zax", "pane", name]).is_ok(),
                "`zax pane {name}` must parse"
            );
        }
    }

    #[test]
    fn pane_exec_takes_everything_after_the_double_dash_as_the_command() {
        let cli = Cli::try_parse_from([
            "zax", "pane", "exec", "--title", "Deploys", "--every", "60s", "--", "kubectl", "get",
            "pods", "-n", "prod",
        ])
        .unwrap();
        match cli.command {
            Commands::Pane {
                command:
                    PaneCommand::Exec {
                        title,
                        every,
                        command,
                    },
            } => {
                assert_eq!(title, "Deploys");
                assert_eq!(every, "60s");
                assert_eq!(command, ["kubectl", "get", "pods", "-n", "prod"]);
            }
            other => panic!("expected pane exec, got {other:?}"),
        }
    }

    #[test]
    fn pane_exec_defaults_its_interval() {
        let cli =
            Cli::try_parse_from(["zax", "pane", "exec", "--title", "T", "--", "true"]).unwrap();
        match cli.command {
            Commands::Pane {
                command: PaneCommand::Exec { every, .. },
            } => {
                assert_eq!(every, "60s");
            }
            other => panic!("expected pane exec, got {other:?}"),
        }
    }

    #[test]
    fn pane_exec_passes_a_double_dash_inside_the_command_through_unchanged() {
        // The command's own arguments may contain a literal `--`; clap's
        // `last = true` capture must not treat it as another separator.
        let cli = Cli::try_parse_from([
            "zax",
            "pane",
            "exec",
            "--title",
            "T",
            "--",
            "sh",
            "-c",
            "echo -- ok",
        ])
        .unwrap();
        match cli.command {
            Commands::Pane {
                command: PaneCommand::Exec { command, .. },
            } => {
                assert_eq!(command, ["sh", "-c", "echo -- ok"]);
            }
            other => panic!("expected pane exec, got {other:?}"),
        }
    }

    #[test]
    fn config_opens_the_modal_and_show_prints_the_same_view() {
        // Bare `zax config` is the modal; `show` is what you want when a session
        // will not start and there is no popup to open one from.
        assert!(matches!(
            Cli::try_parse_from(["zax", "config"]).unwrap().command,
            Commands::Config { command: None }
        ));
        assert!(matches!(
            Cli::try_parse_from(["zax", "config", "show"])
                .unwrap()
                .command,
            Commands::Config {
                command: Some(ConfigCommand::Show)
            }
        ));
    }

    #[test]
    fn doctor_takes_no_arguments() {
        assert!(matches!(
            Cli::try_parse_from(["zax", "doctor"]).unwrap().command,
            Commands::Doctor
        ));
    }
}
