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
        match &check.result {
            Ok(detail) => println!(
                "ok    {:<8} {}",
                check.name,
                detail.lines().next().unwrap_or("")
            ),
            Err(error) => {
                all_ok = false;
                println!("FAIL  {:<8} {error}", check.name);
            }
        }
    }

    if all_ok {
        Ok(())
    } else {
        std::process::exit(1);
    }
}
