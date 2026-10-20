//! `zax config`: the config modal, and the same view printed.
//!
//! Resolution diverges from every other command on purpose. `wiring::resolve`
//! calls `zax_config::load` and fails the command on a config error, which is
//! right for a command that is about to act on the config - and useless for the
//! one command whose job is to fix it. A modal unreachable because of the
//! mistake it exists to repair would be no use at all, so this resolves the
//! layer paths on its own - all three inside a repository, the global one
//! alone outside - without requiring a successful load, and shows the parse
//! error on the modal's error line instead.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use zax_config::Draft;
use zax_domain::settings::{ConfigView, LayerSlot, SettingKey};
use zax_domain::RootRepoName;
use zax_exec::SystemExec;
use zax_infra_git::GitCli;
use zax_tui::{ConfigAction, ConfigModal};

use crate::cli::ConfigCommand;
use crate::screen::Screen;
use crate::{guard, wiring};

/// How long the loop waits for a keystroke before drawing again. There is no
/// I/O to wait on, so this only has to be short enough to feel immediate.
const TICK: Duration = Duration::from_millis(250);

pub fn run(command: Option<ConfigCommand>) -> Result<()> {
    let resolution = Resolution::resolve()?;
    let draft = match &resolution {
        Resolution::Repo {
            config_home,
            main_worktree,
            repo,
            home,
        } => Draft::open(config_home, main_worktree, repo, home)?,
        Resolution::Global { config_home, home } => Draft::open_global(config_home, home)?,
    };

    match command {
        Some(ConfigCommand::Show) => {
            print!("{}", show(&draft.view()));
            Ok(())
        }
        None => {
            let inside_session = guard::inside_multiplexer_now();
            let modal = match resolution {
                Resolution::Repo { .. } => ConfigModal::new(inside_session),
                Resolution::Global { .. } => ConfigModal::global_only(inside_session),
            };
            edit(draft, modal)
        }
    }
}

/// Everything the draft needs, resolved without loading the config.
///
/// A repository gives all three layers to edit. Outside one there is no
/// checkout to read `.zax.toml` from and no key to derive the personal
/// override's path, so the only layer left is the global one - and only
/// when it already turns the `projects` feature on, since that is the one
/// reason to run `zax config` with nothing to configure a session for.
enum Resolution {
    Repo {
        config_home: PathBuf,
        main_worktree: PathBuf,
        repo: RootRepoName,
        home: String,
    },
    Global {
        config_home: PathBuf,
        home: String,
    },
}

impl Resolution {
    fn resolve() -> Result<Self> {
        let cwd = std::env::current_dir().context("could not read the current directory")?;
        let git = GitCli::new(SystemExec, cwd);
        let config_home = wiring::config_home()?;
        let home = wiring::home()?.to_string_lossy().into_owned();

        match git.repo_name() {
            Ok(repo) => {
                let main_worktree = {
                    use zax_app::GitRepo;
                    git.main_worktree()?
                };
                Ok(Self::Repo {
                    config_home,
                    main_worktree,
                    repo,
                    home,
                })
            }
            // Not inside a repository is not itself an error: `projects.root`
            // exists so zax can still find one, and the global layer is the
            // only one that can hold it. A global config that parses decides
            // it, through the one refusal `wiring` owns.
            //
            // One that does not parse decides nothing: it cannot say whether
            // `projects` is on, and this is the command that repairs it, so
            // the modal opens on it with the parse error on its error line
            // rather than the command failing the way every other command
            // rightly does.
            Err(error) => {
                if let Ok(config) = zax_config::load_global(&config_home, &home) {
                    wiring::outside_repository(&config, error)?;
                }
                Ok(Self::Global { config_home, home })
            }
        }
    }
}

/// The pane loop's shape, with no fetch thread: there is nothing to wait on.
fn edit(mut draft: Draft, mut modal: ConfigModal) -> Result<()> {
    let mut screen = Screen::enter()?;

    loop {
        let view = draft.view();
        screen
            .terminal
            .draw(|frame| modal.render(frame, frame.area(), &view))?;

        if event::poll(TICK)? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                let action = modal.handle_key(key, &view);
                if apply(&mut draft, &mut modal, action) == Flow::Close {
                    break;
                }
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    Continue,
    Close,
}

/// Turn one action into an effect on the draft.
///
/// Nothing here decides anything: a failed save is handed back to the modal to
/// show, because the modal owns the screen and the edits are still in the
/// buffer either way.
fn apply(draft: &mut Draft, modal: &mut ConfigModal, action: ConfigAction) -> Flow {
    match action {
        ConfigAction::None => {}
        ConfigAction::Set { slot, key, value } => draft.set(slot, key, value),
        ConfigAction::Clear { slot, key } => draft.clear(slot, key),
        ConfigAction::SetPanes { slot, panes } => draft.set_panes(slot, panes),
        ConfigAction::Save => match draft.save() {
            Ok(saved) => modal.report_saved(saved),
            Err(error) => modal.report_error(error.to_string()),
        },
        ConfigAction::Reload { slot } => {
            if let Err(error) = draft.reload(slot) {
                modal.report_error(error.to_string());
            }
        }
        ConfigAction::Close => return Flow::Close,
    }
    Flow::Continue
}

/// The same view, printed. What you want when a session will not start and
/// there is no popup to open one from.
fn show(view: &ConfigView) -> String {
    let count = view.panes.panes.len();
    let panes = line(
        "layout.panes",
        &format!("{count} pane{}", if count == 1 { "" } else { "s" }),
        view.panes.origin.label(),
    );

    let mut out = String::new();
    for row in &view.settings {
        out.push_str(&line(
            row.key.full_name(),
            &row.display_effective(),
            row.origin.label(),
        ));
        // Where the modal shows it, and where the TOML has it.
        if row.key == SettingKey::AgentCommand {
            out.push_str(&panes);
        }
    }

    out.push('\n');
    for slot in LayerSlot::ALL {
        let Some(layer) = view.layer(slot) else {
            continue;
        };
        out.push_str(&format!(
            "{:<10} {:<50} {}\n",
            slot.label(),
            layer.path.display().to_string(),
            if layer.exists { "" } else { "(absent)" }
        ));
    }

    if let Err(error) = &view.status {
        out.push_str(&format!(
            "\nthis config will not start a session: {error}\n"
        ));
    }
    out
}

fn line(name: &str, value: &str, origin: &str) -> String {
    format!("{name:<16} {value:<34} {origin}\n")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use zax_domain::settings::{Origin, PaneEdit, PaneKindTag, SettingValue};

    use super::*;

    struct Fixture {
        _tmp: tempfile::TempDir,
        config_home: PathBuf,
        main_worktree: PathBuf,
    }

    impl Fixture {
        fn new(repo_layer: Option<&str>) -> Self {
            let tmp = tempfile::TempDir::new().unwrap();
            let config_home = tmp.path().join("config");
            let main_worktree = tmp.path().join("src").join("zax");
            std::fs::create_dir_all(&main_worktree).unwrap();
            if let Some(text) = repo_layer {
                std::fs::write(main_worktree.join(".zax.toml"), text).unwrap();
            }
            Self {
                _tmp: tmp,
                config_home,
                main_worktree,
            }
        }

        fn draft(&self) -> Draft {
            Draft::open(
                &self.config_home,
                &self.main_worktree,
                &RootRepoName::parse("zax").unwrap(),
                "/home/me",
            )
            .unwrap()
        }

        fn personal(&self) -> String {
            std::fs::read_to_string(
                self.config_home
                    .join("zax")
                    .join("repos")
                    .join("zax")
                    .join("config.toml"),
            )
            .unwrap()
        }

        fn repo_text(&self) -> String {
            std::fs::read_to_string(self.main_worktree.join(".zax.toml")).unwrap()
        }

        /// The global layer alone, as it is opened outside a repository.
        fn global_draft(&self) -> Draft {
            Draft::open_global(&self.config_home, "/home/me").unwrap()
        }

        fn global_text(&self) -> String {
            std::fs::read_to_string(zax_config::global_config_path(&self.config_home)).unwrap()
        }
    }

    /// The loop's body, with no terminal: actions in, files out.
    fn drive(draft: &mut Draft, modal: &mut ConfigModal, actions: Vec<ConfigAction>) -> Flow {
        let mut flow = Flow::Continue;
        for action in actions {
            flow = apply(draft, modal, action);
        }
        flow
    }

    #[test]
    fn a_scalar_action_reaches_the_file_when_the_save_action_arrives() {
        let fixture = Fixture::new(None);
        let mut draft = fixture.draft();
        let mut modal = ConfigModal::new(false);

        drive(
            &mut draft,
            &mut modal,
            vec![ConfigAction::Set {
                slot: LayerSlot::Personal,
                key: SettingKey::TmuxPrefix,
                value: SettingValue::Scalar("C-q".to_owned()),
            }],
        );
        // Staged: `s` is what writes.
        assert!(!fixture
            .config_home
            .join("zax/repos/zax/config.toml")
            .exists());

        drive(&mut draft, &mut modal, vec![ConfigAction::Save]);
        assert!(fixture.personal().contains("prefix = \"C-q\""));
    }

    #[test]
    fn a_list_action_reaches_the_file() {
        let fixture = Fixture::new(None);
        let mut draft = fixture.draft();
        let mut modal = ConfigModal::new(false);

        drive(
            &mut draft,
            &mut modal,
            vec![
                ConfigAction::Set {
                    slot: LayerSlot::Personal,
                    key: SettingKey::WorktreeCopy,
                    value: SettingValue::List(vec![".env".to_owned(), ".envrc".to_owned()]),
                },
                ConfigAction::Save,
            ],
        );

        assert!(fixture.personal().contains("copy = [\".env\", \".envrc\"]"));
    }

    #[test]
    fn a_pane_reorder_reaches_the_file_as_the_whole_list() {
        let fixture = Fixture::new(Some(
            "[[layout.panes]]\nkind = \"builtin\"\nname = \"prs-for-review\"\n\n\
             [[layout.panes]]\nkind = \"builtin\"\nname = \"branch-status\"\n",
        ));
        let mut draft = fixture.draft();
        let mut modal = ConfigModal::new(false);

        let mut panes = draft.view().panes.panes;
        panes.swap(0, 1);
        drive(
            &mut draft,
            &mut modal,
            vec![
                ConfigAction::SetPanes {
                    slot: LayerSlot::Repo,
                    panes,
                },
                ConfigAction::Save,
            ],
        );

        let text = fixture.repo_text();
        let first = text.find("branch-status").unwrap();
        let second = text.find("prs-for-review").unwrap();
        assert!(first < second, "got:\n{text}");
    }

    #[test]
    fn clearing_a_setting_removes_it_from_the_file() {
        let fixture = Fixture::new(Some("[tmux]\nprefix = \"C-a\"\n"));
        let mut draft = fixture.draft();
        let mut modal = ConfigModal::new(false);

        drive(
            &mut draft,
            &mut modal,
            vec![
                ConfigAction::Clear {
                    slot: LayerSlot::Repo,
                    key: SettingKey::TmuxPrefix,
                },
                ConfigAction::Save,
            ],
        );

        assert!(!fixture.repo_text().contains("prefix"));
    }

    #[test]
    fn an_invalid_draft_leaves_the_files_alone_and_the_modal_shows_why() {
        let fixture = Fixture::new(None);
        let mut draft = fixture.draft();
        let mut modal = ConfigModal::new(false);

        drive(
            &mut draft,
            &mut modal,
            vec![
                ConfigAction::Set {
                    slot: LayerSlot::Personal,
                    key: SettingKey::PrsMaxAge,
                    value: SettingValue::Scalar("fortnight".to_owned()),
                },
                ConfigAction::Save,
            ],
        );

        assert!(!fixture
            .config_home
            .join("zax/repos/zax/config.toml")
            .exists());
        // The draft still refuses, which is what the modal is showing.
        assert!(draft.view().status.is_err());
    }

    #[test]
    fn a_layer_changed_on_disk_is_reported_and_a_reload_action_takes_it_on_board() {
        let fixture = Fixture::new(Some("[tmux]\nprefix = \"C-a\"\n"));
        let mut draft = fixture.draft();
        let mut modal = ConfigModal::new(false);

        apply(
            &mut draft,
            &mut modal,
            ConfigAction::Set {
                slot: LayerSlot::Repo,
                key: SettingKey::TmuxPrefix,
                value: SettingValue::Scalar("C-q".to_owned()),
            },
        );
        std::fs::write(
            fixture.main_worktree.join(".zax.toml"),
            "[tmux]\nprefix = \"C-z\"\n",
        )
        .unwrap();

        apply(&mut draft, &mut modal, ConfigAction::Save);
        assert!(fixture.repo_text().contains("C-z"), "nothing was written");

        apply(
            &mut draft,
            &mut modal,
            ConfigAction::Reload {
                slot: LayerSlot::Repo,
            },
        );
        assert_eq!(
            draft
                .view()
                .row(SettingKey::TmuxPrefix)
                .unwrap()
                .display_effective(),
            "C-z"
        );
    }

    #[test]
    fn the_close_action_is_the_only_one_that_ends_the_loop() {
        let fixture = Fixture::new(None);
        let mut draft = fixture.draft();
        let mut modal = ConfigModal::new(false);

        assert_eq!(
            apply(&mut draft, &mut modal, ConfigAction::None),
            Flow::Continue
        );
        assert_eq!(
            apply(&mut draft, &mut modal, ConfigAction::Save),
            Flow::Continue
        );
        assert_eq!(
            apply(&mut draft, &mut modal, ConfigAction::Close),
            Flow::Close
        );
    }

    #[test]
    fn show_prints_every_setting_its_origin_and_the_three_paths() {
        let fixture = Fixture::new(Some("[tmux]\nprefix = \"C-a\"\n"));
        let printed = show(&fixture.draft().view());

        assert!(printed.contains("tmux.prefix"), "got:\n{printed}");
        assert!(printed.contains("C-a"), "got:\n{printed}");
        assert!(printed.contains("repo"), "got:\n{printed}");
        // A setting no layer defines still has a value and an origin.
        assert!(printed.contains("worktree.root"), "got:\n{printed}");
        assert!(printed.contains("default"), "got:\n{printed}");
        assert!(printed.contains("layout.panes"), "got:\n{printed}");
        assert!(printed.contains(".zax.toml"), "got:\n{printed}");
        assert!(printed.contains("config.toml"), "got:\n{printed}");
        assert!(printed.contains("(absent)"), "got:\n{printed}");
    }

    #[test]
    fn show_says_when_the_config_will_not_start_a_session() {
        // The case it exists for: no popup to open, because nothing starts.
        let fixture = Fixture::new(Some("[prs]\nmax_age = \"fortnight\"\n"));
        let printed = show(&fixture.draft().view());

        assert!(
            printed.contains("will not start a session"),
            "got:\n{printed}"
        );
        assert!(printed.contains("fortnight"), "got:\n{printed}");
    }

    #[test]
    fn a_pane_list_counts_as_one_row_with_one_origin() {
        let fixture = Fixture::new(None);
        let mut draft = fixture.draft();
        let mut modal = ConfigModal::new(false);

        apply(
            &mut draft,
            &mut modal,
            ConfigAction::SetPanes {
                slot: LayerSlot::Personal,
                panes: vec![PaneEdit {
                    kind: PaneKindTag::Raw,
                    name: None,
                    title: None,
                    every: None,
                    command: Some(vec!["lazygit".to_owned()]),
                    size: None,
                }],
            },
        );

        let view = draft.view();
        assert_eq!(view.panes.origin, Origin::Layer(LayerSlot::Personal));
        assert!(show(&view).contains("1 pane "), "got:\n{}", show(&view));
    }

    #[test]
    fn a_global_only_draft_saves_projects_root() {
        let fixture = Fixture::new(None);
        let mut draft = fixture.global_draft();
        let mut modal = ConfigModal::global_only(false);

        drive(
            &mut draft,
            &mut modal,
            vec![
                ConfigAction::Set {
                    slot: LayerSlot::Global,
                    key: SettingKey::ProjectsRoot,
                    value: SettingValue::Scalar("~/src".to_owned()),
                },
                ConfigAction::Save,
            ],
        );

        assert!(
            fixture.global_text().contains("root = "),
            "got:\n{}",
            fixture.global_text()
        );
    }
}
