//! zax-jira: a zax ticket provider for Jira, through `acli`.
//!
//! zax's broker starts this and speaks the provider protocol to it over
//! stdin and stdout. Diagnostics go to stderr.

mod argv;
mod config;
mod dto;
mod jira;
mod misconfigured;

use std::io::{stdin, stdout, BufReader};

use config::JiraConfig;
use misconfigured::Misconfigured;
use zax_protocol::provider::Provider;

fn main() {
    match load_config() {
        Ok(config) => serve(&mut jira::Jira::new(zax_exec::SystemExec, config)),
        Err(message) => {
            eprintln!("zax-jira: {message}");
            // Serve anyway, so the pane shows what is wrong with the file
            // rather than only that the provider exited.
            serve(&mut Misconfigured::new(message));
        }
    }
}

fn load_config() -> Result<JiraConfig, String> {
    match JiraConfig::default_path() {
        Some(path) => JiraConfig::load(&path),
        None => JiraConfig::from_toml(""),
    }
}

fn serve<P: Provider>(provider: &mut P) {
    if let Err(e) =
        zax_protocol::provider::serve(BufReader::new(stdin().lock()), stdout().lock(), provider)
    {
        eprintln!("zax-jira: {e}");
        std::process::exit(1);
    }
}
