//! `SshAgent` via `ssh-add`.
//!
//! The agent is a credential rather than a tool: zax never asks it anything
//! during normal work, it just needs the key to already be in there when git
//! dials an ssh remote. So the only question this adapter asks is the one
//! `zax doctor` needs - whether that will be true.

use zax_app::{PortError, SshAgent};
use zax_exec::{Exec, ExecError};

const PROGRAM: &str = "ssh-add";
const TOOL: &str = "ssh agent";

/// What `ssh-add -l` means by its exit status. An agent holding nothing and no
/// agent at all are both failures, but they have different cures, so they do
/// not get the same message.
const NO_IDENTITIES: i32 = 1;

pub struct SshAddCli<E: Exec> {
    exec: E,
}

impl<E: Exec> SshAddCli<E> {
    pub fn new(exec: E) -> Self {
        Self { exec }
    }

    /// For tests that assert on the recorded calls.
    pub fn exec_ref(&self) -> &E {
        &self.exec
    }
}

impl<E: Exec> SshAgent for SshAddCli<E> {
    fn probe(&self) -> Result<String, PortError> {
        let out = self
            .exec
            .exec(PROGRAM, &["-l".to_owned()], None, &[])
            .map_err(|e| match e {
                ExecError::NotFound { .. } => PortError::MissingTool { tool: PROGRAM },
                ExecError::Io { detail, .. } => PortError::Incompatible { tool: TOOL, detail },
            })?;

        if out.ok() {
            let keys = out.stdout.lines().filter(|l| !l.trim().is_empty()).count();
            return Ok(format!("{keys} key(s) loaded"));
        }

        Err(PortError::NotAuthenticated {
            tool: TOOL,
            detail: if out.status == NO_IDENTITIES {
                // The case that looks like a working setup until a fetch needs
                // the key: the agent is there, it is just empty.
                "the agent is running but holds no keys\n\
                 run `ssh-add` before using `SPC w o` or `SPC w r`: once a pane has started a \
                 fetch, a locked key can only be unlocked by a dialog"
                    .to_owned()
            } else {
                "no agent to ask, so a locked key cannot be unlocked at all\n\
                 $SSH_AUTH_SOCK is unset or points at nothing that answers; \
                 start one with `ssh-agent`, or point `IdentityAgent` at the \
                 agent your desktop already runs"
                    .to_owned()
            },
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use std::path::Path;

    use zax_exec::{Output, RecordingExec};

    use super::*;

    fn answering(status: i32, stdout: &str, stderr: &str) -> SshAddCli<RecordingExec> {
        SshAddCli::new(RecordingExec::with_responses(vec![Output {
            status,
            stdout: stdout.to_owned(),
            stderr: stderr.to_owned(),
        }]))
    }

    #[test]
    fn an_agent_holding_keys_reports_how_many() {
        let keys = "256 SHA256:abc me@laptop (ED25519)\n\
                    3072 SHA256:def work key (RSA)\n";
        let ssh = answering(0, keys, "");

        assert_eq!(ssh.probe().unwrap(), "2 key(s) loaded");
        assert_eq!(ssh.exec_ref().args(0), ["-l"]);
    }

    #[test]
    fn an_empty_agent_is_told_apart_from_a_missing_one() {
        // This is the failure that looks like a working setup: the agent
        // answers, so nothing seems wrong until a fetch needs the key.
        let ssh = answering(1, "", "The agent has no identities.\n");

        let error = ssh.probe().unwrap_err().to_string();

        assert!(error.contains("holds no keys"), "got: {error}");
        assert!(error.contains("ssh-add"), "got: {error}");
    }

    #[test]
    fn no_agent_at_all_says_where_to_look() {
        let ssh = answering(
            2,
            "",
            "Could not open a connection to your authentication agent.\n",
        );

        let error = ssh.probe().unwrap_err().to_string();

        assert!(error.contains("SSH_AUTH_SOCK"), "got: {error}");
        assert!(!error.contains("holds no keys"), "got: {error}");
    }

    #[test]
    fn a_machine_without_ssh_add_reports_the_missing_tool() {
        struct Missing;

        impl Exec for Missing {
            fn exec(
                &self,
                program: &str,
                _args: &[String],
                _cwd: Option<&Path>,
                _env: &[zax_exec::EnvVar],
            ) -> Result<Output, ExecError> {
                Err(ExecError::NotFound {
                    program: program.to_owned(),
                })
            }
        }

        let error = SshAddCli::new(Missing).probe().unwrap_err();

        assert!(matches!(error, PortError::MissingTool { tool: "ssh-add" }));
    }
}
