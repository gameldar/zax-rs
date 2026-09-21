//! `zax doctor`.

use anyhow::Result;
use zax_app::queries;

use crate::wiring;

pub fn run() -> Result<()> {
    let ctx = wiring::resolve()?;
    let tmux = wiring::tmux(&ctx.config)?;

    let checks = queries::doctor(&tmux, &ctx.git, &ctx.gh);
    let mut all_ok = true;

    for check in &checks {
        // A check's detail can run to several lines: a failure carries the
        // context that explains it. Printing only the first line throws away
        // the part that tells the reader what to do about it.
        match &check.result {
            // A working check needs one line to say so. `gh auth status` in
            // particular prints an account dump nobody asked to see.
            Ok(detail) => println!(
                "ok    {:<8} {}",
                check.name,
                detail.lines().next().unwrap_or("")
            ),
            Err(error) => {
                all_ok = false;
                print_failure(check.name, &error.to_string());
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
    println!("FAIL  {name:<8} {}", lines.next().unwrap_or(""));
    for line in lines {
        println!("               {}", line.trim_start());
    }
}
