//! The provider: acli behind the protocol's four methods.

use std::collections::BTreeMap;

use zax_exec::{Exec, ExecError, Output};
use zax_protocol::provider::Provider;
use zax_protocol::{
    AssignedResult, Capabilities, ErrorKind, InitializeResult, RpcError, TransitionsResult,
    WireTransition, PROTOCOL_VERSION,
};

use crate::argv;
use crate::config::JiraConfig;
use crate::dto::{parse_search, parse_site};

const ACLI: &str = "acli";

pub struct Jira<E: Exec> {
    exec: E,
    config: JiraConfig,
    site: Option<String>,
    /// The status each ticket was in at the last `assigned`, so its current
    /// status can be left out of the moves offered.
    statuses: BTreeMap<String, String>,
}

impl<E: Exec> Jira<E> {
    pub fn new(exec: E, config: JiraConfig) -> Self {
        Self {
            exec,
            config,
            site: None,
            statuses: BTreeMap::new(),
        }
    }

    fn acli(&self, args: Vec<String>) -> Result<Output, RpcError> {
        self.exec.exec(ACLI, &args, None, &[]).map_err(|e| match e {
            ExecError::NotFound { .. } => RpcError::new(
                ErrorKind::MissingTool,
                "acli is not installed or not on PATH; see https://developer.atlassian.com/cloud/acli/",
            ),
            ExecError::Io { detail, .. } => {
                RpcError::new(ErrorKind::Unavailable, format!("could not run acli: {detail}"))
            }
        })
    }

    fn acli_ok(&self, args: Vec<String>) -> Result<String, RpcError> {
        let out = self.acli(args)?;
        if out.ok() {
            Ok(out.stdout)
        } else {
            Err(RpcError::new(ErrorKind::Unavailable, last_line(&out)))
        }
    }

    fn site(&mut self) -> Result<String, RpcError> {
        if let Some(site) = &self.site {
            return Ok(site.clone());
        }
        let out = self.acli(argv::auth_status())?;
        if !out.ok() {
            return Err(RpcError::new(
                ErrorKind::NotAuthenticated,
                format!(
                    "acli is not logged in to Jira ({}); run `acli jira auth login`",
                    last_line(&out)
                ),
            ));
        }
        let site = parse_site(&out.stdout).ok_or_else(|| {
            RpcError::new(
                ErrorKind::NotAuthenticated,
                "acli jira auth status did not name a site",
            )
        })?;
        self.site = Some(site.clone());
        Ok(site)
    }
}

fn last_line(out: &Output) -> String {
    out.stderr
        .lines()
        .chain(out.stdout.lines())
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("acli exited with status {}", out.status))
}

impl<E: Exec> Provider for Jira<E> {
    fn initialize(&mut self) -> Result<InitializeResult, RpcError> {
        self.site()?;
        Ok(InitializeResult {
            protocol: PROTOCOL_VERSION,
            name: "Jira".to_owned(),
            capabilities: Capabilities { transitions: true },
            poll: Some("2m".to_owned()),
        })
    }

    fn assigned(&mut self) -> Result<AssignedResult, RpcError> {
        let site = self.site()?;
        let json = self.acli_ok(argv::search(&self.config.jql))?;
        let (tickets, dropped) =
            parse_search(&json, &site).map_err(|e| RpcError::new(ErrorKind::Unavailable, e))?;
        for line in dropped {
            eprintln!("zax-jira: dropped {line}");
        }
        self.statuses = tickets
            .iter()
            .map(|t| (t.key.clone(), t.status.name.clone()))
            .collect();
        Ok(AssignedResult { tickets })
    }

    fn transitions(&mut self, key: &str) -> Result<TransitionsResult, RpcError> {
        let current = self.statuses.get(key);
        let transitions = self
            .config
            .transitions_for(key)
            .iter()
            .filter(|name| Some(*name) != current)
            .map(|name| WireTransition {
                id: name.clone(),
                name: name.clone(),
            })
            .collect();
        Ok(TransitionsResult { transitions })
    }

    fn transition(&mut self, key: &str, id: &str) -> Result<(), RpcError> {
        self.acli_ok(argv::transition(key, id)).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;
    use zax_exec::RecordingExec;

    fn ok(stdout: &str) -> Output {
        Output {
            status: 0,
            stdout: stdout.into(),
            stderr: String::new(),
        }
    }

    fn jira(responses: Vec<Output>) -> Jira<RecordingExec> {
        Jira::new(
            RecordingExec::with_responses(responses),
            JiraConfig::from_toml("").unwrap(),
        )
    }

    #[test]
    fn initialize_reads_the_site_and_advertises_transitions() {
        let mut j = jira(vec![ok(include_str!("../tests/fixtures/auth_status.txt"))]);
        let init = j.initialize().unwrap();
        assert_eq!(init.name, "Jira");
        assert!(init.capabilities.transitions);
        assert_eq!(init.poll.as_deref(), Some("2m"));
    }

    #[test]
    fn not_logged_in_is_not_authenticated() {
        let mut j = jira(vec![Output {
            status: 1,
            stdout: String::new(),
            stderr: "not logged in".into(),
        }]);
        assert_eq!(
            j.initialize().unwrap_err().kind(),
            Some(ErrorKind::NotAuthenticated)
        );
    }

    #[test]
    fn missing_acli_is_missing_tool() {
        struct Missing;
        impl zax_exec::Exec for Missing {
            fn exec(
                &self,
                program: &str,
                _: &[String],
                _: Option<&std::path::Path>,
                _: &[zax_exec::EnvVar],
            ) -> Result<Output, ExecError> {
                Err(ExecError::NotFound {
                    program: program.into(),
                })
            }
        }
        let mut j = Jira::new(Missing, JiraConfig::from_toml("").unwrap());
        let err = j.initialize().unwrap_err();
        assert_eq!(err.kind(), Some(ErrorKind::MissingTool));
        assert!(err.message.contains("acli"), "{}", err.message);
    }

    #[test]
    fn transitions_leave_out_the_status_the_ticket_is_in() {
        let mut j = jira(vec![
            ok(include_str!("../tests/fixtures/auth_status.txt")),
            ok(include_str!("../tests/fixtures/search.json")),
        ]);
        j.initialize().unwrap();
        j.assigned().unwrap();
        let names: Vec<String> = j
            .transitions("ACME-12")
            .unwrap()
            .transitions
            .into_iter()
            .map(|t| t.name)
            .collect();
        assert_eq!(names, ["In Progress", "Done"]);
    }

    #[test]
    fn a_refused_transition_reports_acli_s_own_words() {
        let mut j = jira(vec![Output {
            status: 1,
            stdout: String::new(),
            stderr: "transition not allowed\n".into(),
        }]);
        let err = j.transition("ACME-12", "Done").unwrap_err();
        assert_eq!(err.kind(), Some(ErrorKind::Unavailable));
        assert!(err.message.contains("transition not allowed"));
    }
}
