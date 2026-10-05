//! `zax doctor`.

use anyhow::Result;
use zax_app::queries;
use zax_infra_tmux::diagnostics;

use crate::wiring;

pub fn run() -> Result<()> {
    let ctx = wiring::resolve()?;
    let tmux = wiring::tmux(&ctx.config)?;

    let editor = wiring::editor(&ctx.config);

    let checks = queries::doctor(&tmux, &ctx.git, &ctx.gh, &editor, &wiring::ssh());
    let mut all_ok = true;

    for check in &checks {
        // A check's detail can run to several lines: a failure carries the
        // context that explains it. Printing only the first line throws away
        // the part that tells the reader what to do about it.
        match &check.result {
            // A working check needs one line to say so. `gh auth status` in
            // particular prints an account dump nobody asked to see.
            Ok(detail) => {
                let first = detail.lines().next().unwrap_or("");
                println!("ok    {:<9} {}", check.name, first);
                // A working tmux can still be too old for the config popup,
                // which is a note about one binding rather than a failure.
                if check.name == "tmux" {
                    if let Some(hint) = diagnostics::popup_hint(first) {
                        println!("note            {hint}");
                    }
                }
            }
            Err(error) => {
                all_ok = false;
                print_failure(check.name, &error.to_string());
            }
        }
    }

    // A ticket provider is optional, so its line appears only once one is
    // configured. Starting it and reading its `initialize` answer is the one
    // check that covers a wrong path, a bad build and a protocol mismatch.
    if let Some(provider) = &ctx.config.tickets.provider {
        // Probed exactly as the broker will run it, relative path anchored.
        let main_worktree = {
            use zax_app::GitRepo;
            ctx.git.main_worktree()?
        };
        let home = crate::wiring::home().ok();
        let argv =
            crate::cmd_tickets::anchor_provider(provider.argv(), &main_worktree, home.as_deref());
        match zax_broker::provider_source::probe(argv) {
            Ok(detail) => println!("ok    {:<8} {detail}", "tickets"),
            Err(error) => {
                all_ok = false;
                print_failure("tickets", &error);
            }
        }
    }

    if all_ok {
        Ok(())
    } else {
        std::process::exit(1);
    }
}

/// A failure, with its context lined up underneath. The context is the part
/// that says what to do about it, so it is never truncated.
fn print_failure(name: &str, detail: &str) {
    let mut lines = detail.lines();
    println!("FAIL  {name:<9} {}", lines.next().unwrap_or(""));
    for line in lines {
        println!("                {}", line.trim_start());
    }
}
