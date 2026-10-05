//! `zax broker`: the composition root for the shared broker.
//!
//! The broker crate is generic; this is where `tickets` and `work` keys are
//! given meaning.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use serde_json::Value;
use zax_app::queries;
use zax_broker::client::binary_identity;
use zax_broker::provider_source::ProviderSource;
use zax_broker::server::{self, ServerConfig};
use zax_broker::topics::{encode_work, TicketsSpec, WorkSpec, TICKETS, WORK};
use zax_broker::{BrokerPaths, Source, SourceFactory};
use zax_domain::WorktreeRoot;
use zax_exec::{SystemExec, SystemHandover};
use zax_infra_tmux::{TmuxCli, SOCKET_DEFAULT};
use zax_protocol::SourceKey;

use crate::wiring;

/// How long a broker with no panes lingers, so a pane restarted by a session
/// rebuild finds the cache still warm.
const IDLE_EXIT: Duration = Duration::from_secs(30);
const WORK_INTERVAL: Duration = Duration::from_secs(60);

pub fn run() -> Result<()> {
    // The broker outlives the pane that spawned it and serves every
    // repository, so it must not hold that pane's worktree as its cwd: the
    // directory could be removed, and nothing it runs should depend on it.
    std::env::set_current_dir(wiring::home()?)
        .map_err(|e| anyhow!("could not change to the home directory: {e}"))?;
    // `ZAX_BROKER_DIR` puts socket, lock and log in one directory. It exists
    // only so tests can run a broker without touching the real one; nothing
    // in zax itself sets it.
    let paths = match std::env::var_os("ZAX_BROKER_DIR") {
        Some(dir) => BrokerPaths::in_dir(&PathBuf::from(&dir), &PathBuf::from(&dir)),
        None => BrokerPaths::resolve().map_err(|e| anyhow!(e))?,
    };
    let identity = binary_identity(&wiring::zax_bin()?);
    // Stderr is the broker log. A zero identity is not refused, but every
    // pane built from a binary whose time can be read will see this broker
    // as older and restart it, so the log should say why.
    if identity == 0 {
        eprintln!(
            "zax broker: warning: could not read this binary's modification time; \
             any pane will restart this broker"
        );
    }
    eprintln!("zax broker: listening on {}", paths.socket.display());
    server::run(
        ServerConfig {
            paths,
            identity,
            idle_exit: IDLE_EXIT,
        },
        Arc::new(Factory),
    )?;
    Ok(())
}

struct Factory;

impl SourceFactory for Factory {
    fn create(&self, key: &SourceKey) -> Result<Box<dyn Source>, String> {
        match key.topic.as_str() {
            TICKETS => {
                let spec: TicketsSpec =
                    serde_json::from_str(&key.source).map_err(|e| e.to_string())?;
                Ok(Box::new(ProviderSource::new(spec.provider)))
            }
            WORK => {
                let spec: WorkSpec =
                    serde_json::from_str(&key.source).map_err(|e| e.to_string())?;
                let root = WorktreeRoot::new(PathBuf::from(spec.worktree_root))
                    .map_err(|e| e.to_string())?;
                Ok(Box::new(WorkSource { root }))
            }
            other => Err(format!("the broker has no `{other}` source")),
        }
    }
}

/// Every zax worktree under the root, the sessions running in them and the
/// pull requests on their branches: what the tickets pane joins tickets to.
struct WorkSource {
    root: WorktreeRoot,
}

impl Source for WorkSource {
    fn fetch(&mut self) -> Result<Value, String> {
        // The adapters are rebuilt per fetch: they are a few paths and hold
        // no state worth keeping between polls a minute apart.
        let home = wiring::home().map_err(|e| e.to_string())?;
        let gh = zax_infra_github::GhCli::new(SystemExec, home);
        let tmux = TmuxCli::new(
            SystemExec,
            SystemHandover,
            SOCKET_DEFAULT.to_owned(),
            // Only read when starting a server, which listing sessions never
            // does, so the broker need not render one.
            wiring::runtime_dir()
                .map_err(|e| e.to_string())?
                .join("tmux.conf"),
        );
        let work = queries::list_work(&zax_infra_git::FsWorktreeScanner, &tmux, &gh, &self.root)
            .map_err(|e| e.to_string())?;
        Ok(encode_work(&work))
    }

    fn call(&mut self, method: &str, _params: Value) -> Result<Value, String> {
        Err(format!("the work source has no `{method}`"))
    }

    fn interval(&self) -> Duration {
        WORK_INTERVAL
    }
}
