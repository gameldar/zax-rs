//! `zax worktree new | review | list | rm`.

use std::path::{Path, PathBuf};

use anyhow::Result;
use zax_app::{
    queries, CreateReviewWorktree, CreateReviewWorktreeRequest, CreateWorktree,
    CreateWorktreeRequest, Provisioning, ResolveCheckout, WorktreeOpener,
};
use zax_domain::{BranchName, PrRef, RepoTarget, WorktreeRoot};

use crate::cli::WorktreeCommand;
use crate::marked;
use crate::wiring::{self, RealFs, RealRunner};

pub fn run(command: WorktreeCommand) -> Result<()> {
    match command {
        WorktreeCommand::New {
            branch,
            from,
            no_hooks,
            no_open,
            repo,
        } => new(branch, from, no_hooks, no_open, repo),
        WorktreeCommand::Review {
            pr,
            no_hooks,
            force,
            no_open,
            repo,
        } => review(pr, no_hooks, force, no_open, repo),
        WorktreeCommand::List => list(),
        WorktreeCommand::Rm { branch, force } => remove(branch, force),
    }
}

/// `worktree.root` is always the session's own: new worktrees have to land
/// where this session's tickets pane and `W` popup look for them, whichever
/// repository they belong to. `copy` and `hooks` are the target repository's
/// own setup, since that is what its worktree actually needs copied and run.
fn provisioning_and_root(
    session: &zax_config::Config,
    target: &zax_config::Config,
    run_hooks: bool,
) -> (WorktreeRoot, Provisioning) {
    (
        session.worktree.root.clone(),
        Provisioning {
            copy: target.worktree.copy.clone(),
            hooks: target.worktree.hooks.clone(),
            run_hooks,
        },
    )
}

/// `--repo`, parsed once at the boundary into a `RepoTarget`. A leading `~`
/// is expanded here, since `RepoTarget::parse` deliberately leaves that to
/// the caller that knows the home directory.
fn parse_repo(repo: Option<String>, home: &Path) -> Result<Option<RepoTarget>> {
    let Some(raw) = repo else {
        return Ok(None);
    };
    let target = match RepoTarget::parse(&raw)? {
        RepoTarget::Path(path) => RepoTarget::Path(expand_tilde(&path, home)),
        slug => slug,
    };
    Ok(Some(target))
}

/// Both spellings `zax-config`'s `expand_home` accepts: `~/` and a bare `~`,
/// so `--repo ~` means on the command line what it means in a config file.
fn expand_tilde(path: &Path, home: &Path) -> PathBuf {
    match path.to_str() {
        Some("~") => home.to_owned(),
        Some(text) => match text.strip_prefix("~/") {
            Some(rest) => home.join(rest),
            None => path.to_owned(),
        },
        None => path.to_owned(),
    }
}

fn new(
    branch: String,
    from: Option<String>,
    no_hooks: bool,
    no_open: bool,
    repo: Option<String>,
) -> Result<()> {
    let ctx = wiring::resolve()?;
    let branch = BranchName::parse_local(&branch)?;
    let repo = parse_repo(repo, &ctx.home)?;

    let checkouts = wiring::checkouts();
    let checkout = ResolveCheckout::new(&checkouts, ctx.config.projects.as_ref())
        .for_new(repo.as_ref(), ctx.here())?;
    let target = wiring::target_at(&ctx, checkout.as_ref())?;
    let (root, provisioning) =
        provisioning_and_root(&ctx.config, &target.provisioning_config, !no_hooks);

    let outcome =
        CreateWorktree::new(&target.git, &RealFs, &RealRunner).run(CreateWorktreeRequest {
            branch,
            base: from,
            root,
            provisioning,
        })?;

    report(&outcome);
    warn(open_worktree(
        &wiring::opener(&ctx.config),
        outcome.path.as_path(),
        !no_open,
    ));
    Ok(())
}

fn review(
    pr: String,
    no_hooks: bool,
    force: bool,
    no_open: bool,
    repo: Option<String>,
) -> Result<()> {
    let ctx = wiring::resolve()?;
    let pr = PrRef::parse(&pr)?;
    let repo = parse_repo(repo, &ctx.home)?;

    let checkouts = wiring::checkouts();
    let checkout = ResolveCheckout::new(&checkouts, ctx.config.projects.as_ref()).for_review(
        &pr,
        repo.as_ref(),
        ctx.here(),
    )?;
    let target = wiring::target_at(&ctx, checkout.as_ref())?;
    let (root, provisioning) =
        provisioning_and_root(&ctx.config, &target.provisioning_config, !no_hooks);

    let outcome = CreateReviewWorktree::new(&target.git, &target.gh, &RealFs, &RealRunner).run(
        CreateReviewWorktreeRequest {
            pr,
            root,
            provisioning,
            force,
        },
    )?;

    // Before the path, because it is the one line here that reports something
    // lost rather than something made.
    if let Some(forced) = &outcome.forced_update {
        println!(
            "moved branch {} off its own history: {} -> {}",
            forced.branch, forced.previous_tip, forced.head
        );
    }
    report(&outcome.worktree);
    warn(open_review_worktree(
        &wiring::opener(&ctx.config),
        outcome.worktree.path.as_path(),
        &outcome.pr_url,
        !no_open,
    ));
    Ok(())
}

/// Hand the finished worktree to the opener, after the copies and the hooks:
/// what opens is a worktree that is ready to work in, not a bare checkout
/// that grows its config files while you look at it.
///
/// The returned line is a warning, never an error. The worktree was made
/// either way, and the review pane reads a non-zero exit as "the review was
/// refused", which an opener that would not start does not make true.
fn open_worktree<O: WorktreeOpener>(opener: &O, path: &Path, open: bool) -> Option<String> {
    if !open {
        return None;
    }
    opener.open_worktree(path).err().map(|e| e.to_string())
}

/// As `open_worktree`, but the opener is also asked to start the review of
/// `pr_url`, which an opener that can run an agent does in the new space.
fn open_review_worktree<O: WorktreeOpener>(
    opener: &O,
    path: &Path,
    pr_url: &str,
    open: bool,
) -> Option<String> {
    if !open {
        return None;
    }
    opener
        .open_for_review(path, pr_url)
        .err()
        .map(|e| e.to_string())
}

/// Marked as zax's own, because the review pane runs this command as a
/// subprocess and an unmarked line is indistinguishable from a hook's chatter.
fn warn(message: Option<String>) {
    let Some(message) = message else { return };
    marked::warn(&format!("could not open the worktree: {message}"));
}

fn report(outcome: &zax_app::CreateWorktreeOutcome) {
    println!("{}", outcome.path.as_path().display());
    for path in &outcome.report.copied {
        println!("  copied {}", path.display());
    }
    for path in &outcome.report.skipped {
        println!(
            "  skipped {} (not present in the source repo)",
            path.display()
        );
    }
    if outcome.report.hooks_run > 0 {
        println!("  ran {} setup hook(s)", outcome.report.hooks_run);
    }
}

fn list() -> Result<()> {
    let ctx = wiring::resolve()?;
    let repo = ctx.require_repo()?;
    let entries = queries::list_worktrees(&repo.git, &ctx.config.worktree.root, &repo.name)?;
    if entries.is_empty() {
        println!("no zax worktrees for {}", repo.name);
    }
    for entry in entries {
        let branch = entry
            .branch
            .as_ref()
            .map(|b| b.as_str().to_owned())
            .unwrap_or_else(|| "(detached)".to_owned());
        println!("{:<40} {}", branch, entry.path.display());
    }
    Ok(())
}

fn remove(branch: String, force: bool) -> Result<()> {
    let ctx = wiring::resolve()?;
    // Parsed leniently: an old branch that breaks the rule can still be removed.
    let branch = BranchName::parse_foreign(&branch)?;
    let path = queries::remove_worktree(
        &ctx.require_repo()?.git,
        &RealFs,
        &ctx.config.worktree.root,
        &branch,
        force,
    )?;
    println!("removed {}", path.as_path().display());
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use std::path::{Path, PathBuf};

    use zax_app::testing::FakeOpener;
    use zax_app::PortError;
    use zax_domain::{RepoTarget, WorktreeRoot};

    use super::{open_review_worktree, open_worktree, parse_repo, provisioning_and_root};

    fn config() -> zax_config::Config {
        zax_config::parse::from_raw(zax_config::raw::RawConfig::default(), "/h").unwrap()
    }

    #[test]
    fn a_repo_path_has_its_tilde_expanded_against_the_home_directory() {
        let home = Path::new("/h");

        assert_eq!(
            parse_repo(Some("~/src/omega".to_owned()), home).unwrap(),
            Some(RepoTarget::Path(PathBuf::from("/h/src/omega")))
        );
        // A bare `~` is the home directory itself, as the config layer reads
        // it; left alone it reaches the inspector as a literal `~` and comes
        // back as "absent".
        assert_eq!(
            parse_repo(Some("~".to_owned()), home).unwrap(),
            Some(RepoTarget::Path(PathBuf::from("/h")))
        );
        assert_eq!(
            parse_repo(Some("/srv/omega".to_owned()), home).unwrap(),
            Some(RepoTarget::Path(PathBuf::from("/srv/omega")))
        );
        assert_eq!(
            parse_repo(Some("acme/omega".to_owned()), home).unwrap(),
            Some(RepoTarget::Slug(
                zax_domain::RepoSlug::parse("acme/omega").unwrap()
            ))
        );
        assert_eq!(parse_repo(None, home).unwrap(), None);
    }

    #[test]
    fn copy_and_hooks_come_from_the_target_but_the_root_from_the_session() {
        let target_hook = zax_domain::Command::new(vec!["target-hook".to_owned()]).unwrap();

        let mut session = config();
        session.worktree.root = WorktreeRoot::new(PathBuf::from("/w")).unwrap();
        session.worktree.hooks =
            vec![zax_domain::Command::new(vec!["session-hook".to_owned()]).unwrap()];

        let mut target = config();
        target.worktree.root = WorktreeRoot::new(PathBuf::from("/other")).unwrap();
        target.worktree.copy = vec![PathBuf::from(".env")];
        target.worktree.hooks = vec![target_hook.clone()];

        let (root, provisioning) = provisioning_and_root(&session, &target, true);

        assert_eq!(root, WorktreeRoot::new(PathBuf::from("/w")).unwrap());
        assert_eq!(provisioning.copy, vec![PathBuf::from(".env")]);
        assert_eq!(provisioning.hooks, vec![target_hook]);
    }

    #[test]
    fn a_finished_worktree_is_handed_to_the_opener() {
        let opener = FakeOpener::default();

        let warning = open_worktree(&opener, Path::new("/src/worktrees/acme/patch-1"), true);

        assert_eq!(warning, None);
        assert_eq!(
            *opener.opened.borrow(),
            vec![PathBuf::from("/src/worktrees/acme/patch-1")]
        );
    }

    #[test]
    fn a_review_worktree_is_handed_over_with_its_pull_request() {
        let opener = FakeOpener::default();
        let url = "https://github.com/acme/omega/pull/1963";

        let warning = open_review_worktree(
            &opener,
            Path::new("/src/worktrees/acme/patch-1"),
            url,
            true,
        );

        assert_eq!(warning, None);
        assert_eq!(*opener.reviewed.borrow(), vec![url.to_owned()]);
        assert_eq!(
            *opener.opened.borrow(),
            vec![PathBuf::from("/src/worktrees/acme/patch-1")]
        );
    }

    #[test]
    fn no_open_starts_no_review_either() {
        let opener = FakeOpener::default();

        open_review_worktree(
            &opener,
            Path::new("/src/worktrees/acme/patch-1"),
            "https://github.com/acme/omega/pull/1963",
            false,
        );

        assert!(opener.reviewed.borrow().is_empty());
        assert!(opener.opened.borrow().is_empty());
    }

    #[test]
    fn no_open_leaves_the_opener_alone() {
        let opener = FakeOpener::default();

        open_worktree(&opener, Path::new("/src/worktrees/acme/patch-1"), false);

        assert!(opener.opened.borrow().is_empty());
    }

    #[test]
    fn an_opener_that_will_not_start_is_a_warning_and_not_a_failed_command() {
        // The worktree exists either way, and the pane reads a non-zero exit
        // as "the review was refused", which would be a lie.
        let opener = FakeOpener::failing(PortError::MissingProgram {
            program: "zed".to_owned(),
            hint: "run `cli: install`".to_owned(),
        });

        let warning = open_worktree(&opener, Path::new("/src/worktrees/acme/patch-1"), true)
            .expect("a failure the user cannot see is a keystroke that did nothing");

        assert!(warning.contains("zed"), "got: {warning}");
        assert!(warning.contains("cli: install"), "got: {warning}");
    }
}
