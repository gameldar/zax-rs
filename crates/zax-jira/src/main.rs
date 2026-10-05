//! zax-jira: a zax ticket provider for Jira, through `acli`.
//!
//! zax's broker starts this and speaks the provider protocol to it over
//! stdin and stdout. Diagnostics go to stderr.

mod argv;
mod config;
mod dto;
mod jira;

use std::io::{stdin, stdout, BufReader};

use config::JiraConfig;

fn main() {
    let config = match JiraConfig::default_path().map(|p| JiraConfig::load(&p)) {
        Some(Ok(config)) => config,
        Some(Err(e)) => {
            eprintln!("zax-jira: {e}");
            std::process::exit(2);
        }
        None => match JiraConfig::from_toml("") {
            Ok(c) => c,
            Err(e) => {
                eprintln!("zax-jira: {e}");
                std::process::exit(2);
            }
        },
    };
    let mut provider = jira::Jira::new(zax_exec::SystemExec, config);
    if let Err(e) = zax_protocol::provider::serve(
        BufReader::new(stdin().lock()),
        stdout().lock(),
        &mut provider,
    ) {
        eprintln!("zax-jira: {e}");
        std::process::exit(1);
    }
}
