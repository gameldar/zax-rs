//! `zax worktree new | review | list | rm`.

use anyhow::Result;
use zax_app::{
    queries, CreateReviewWorktree, CreateReviewWorktreeRequest, CreateWorktree,
    CreateWorktreeRequest, Provisioning,
};
use zax_domain::{BranchName, PrRef};

use crate::cli::WorktreeCommand;
use crate::wiring::{self, RealFs, RealRunner};

pub fn run(command: WorktreeCommand) -> Result<()> {
    match command {
        WorktreeCommand::New {
            branch,
            from,
            no_hooks,
        } => new(branch, from, no_hooks),
        WorktreeCommand::Review { pr, no_hooks } => review(pr, no_hooks),
        WorktreeCommand::List => list(),
        WorktreeCommand::Rm { branch, force } => remove(branch, force),
    }
}

fn provisioning(config: &zax_config::Config, run_hooks: bool) -> Provisioning {
    Provisioning {
        copy: config.worktree.copy.clone(),
        hooks: config.worktree.hooks.clone(),
        run_hooks,
    }
}

fn new(branch: String, from: Option<String>, no_hooks: bool) -> Result<()> {
    let ctx = wiring::resolve()?;
    let branch = BranchName::parse_local(&branch)?;

    let outcome =
        CreateWorktree::new(&ctx.git, &RealFs, &RealRunner).run(CreateWorktreeRequest {
            branch,
            base: from,
            root: ctx.config.worktree.root.clone(),
            provisioning: provisioning(&ctx.config, !no_hooks),
        })?;

    report(&outcome);
    Ok(())
}

fn review(pr: String, no_hooks: bool) -> Result<()> {
    let ctx = wiring::resolve()?;
    let pr = PrRef::parse(&pr)?;

    let outcome = CreateReviewWorktree::new(&ctx.git, &ctx.gh, &RealFs, &RealRunner).run(
        CreateReviewWorktreeRequest {
            pr,
            root: ctx.config.worktree.root.clone(),
            provisioning: provisioning(&ctx.config, !no_hooks),
        },
    )?;

    report(&outcome);
    Ok(())
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
    let entries = queries::list_worktrees(&ctx.git, &ctx.config.worktree.root, &ctx.repo)?;
    if entries.is_empty() {
        println!("no zax worktrees for {}", ctx.repo);
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
    let path =
        queries::remove_worktree(&ctx.git, &RealFs, &ctx.config.worktree.root, &branch, force)?;
    println!("removed {}", path.as_path().display());
    Ok(())
}
