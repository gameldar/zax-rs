//! A ticket provider as a broker source: a child process speaking JSON-RPC
//! over its stdin and stdout.
//!
//! The child is started on first use and kept. A child that exits, closes
//! its output or does not answer in time is killed, and the next start waits
//! out an exponential backoff, so a broken provider is retried without being
//! hammered. The last good value is the hub's to keep; this only reports.

use std::io::{BufRead, BufReader};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use zax_protocol::{
    method, write_message, AssignedResult, FromProvider, InitializeParams, InitializeResult,
    Request, TicketsTopic, PROTOCOL_VERSION,
};

use crate::source::Source;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_POLL: Duration = Duration::from_secs(120);
const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(300);

struct Running {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    init: InitializeResult,
}

pub struct ProviderSource {
    argv: Vec<String>,
    timeout: Duration,
    running: Option<Running>,
    next_id: u64,
    backoff: Duration,
    retry_at: Option<Instant>,
    poll: Duration,
}

impl ProviderSource {
    pub fn new(argv: Vec<String>) -> Self {
        Self {
            argv,
            timeout: DEFAULT_TIMEOUT,
            running: None,
            next_id: 1,
            backoff: BACKOFF_MIN,
            retry_at: None,
            poll: DEFAULT_POLL,
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    fn program(&self) -> &str {
        self.argv.first().map(String::as_str).unwrap_or("provider")
    }

    fn ensure_running(&mut self) -> Result<(), String> {
        if self.running.is_some() {
            return Ok(());
        }
        if let Some(at) = self.retry_at {
            if Instant::now() < at {
                return Err(format!("{} failed; restarting shortly", self.program()));
            }
        }
        match self.start() {
            Ok(running) => {
                self.poll = running
                    .init
                    .poll
                    .as_deref()
                    .and_then(|p| zax_domain::parse_duration(p).ok())
                    .unwrap_or(DEFAULT_POLL);
                self.running = Some(running);
                self.backoff = BACKOFF_MIN;
                self.retry_at = None;
                Ok(())
            }
            Err(e) => {
                self.fail();
                Err(e)
            }
        }
    }

    fn start(&mut self) -> Result<Running, String> {
        let (program, args) = self.argv.split_first().ok_or("no provider command")?;
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    format!("{program} is not installed or not on PATH")
                } else {
                    format!("could not start {program}: {e}")
                }
            })?;
        let stdin = child.stdin.take().ok_or("provider has no stdin")?;
        let stdout = child.stdout.take().ok_or("provider has no stdout")?;
        let (tx, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let mut running = Running {
            child,
            stdin,
            lines,
            init: InitializeResult {
                protocol: 0,
                name: String::new(),
                capabilities: Default::default(),
                poll: None,
            },
        };
        let params = serde_json::to_value(InitializeParams {
            protocol: PROTOCOL_VERSION,
        })
        .map_err(|e| e.to_string())?;
        let value = request(
            &mut running,
            &mut self.next_id,
            self.timeout,
            program,
            method::INITIALIZE,
            params,
        )?;
        let init: InitializeResult = serde_json::from_value(value).map_err(|e| {
            format!("{program} answered initialize in a shape zax does not know: {e}")
        })?;
        if init.protocol != PROTOCOL_VERSION {
            let _ = running.child.kill();
            let _ = running.child.wait();
            return Err(format!(
                "{} speaks protocol {}, zax speaks {PROTOCOL_VERSION}",
                init.name, init.protocol
            ));
        }
        running.init = init;
        Ok(running)
    }

    /// Kill the child and schedule the next start.
    fn fail(&mut self) {
        if let Some(mut running) = self.running.take() {
            let _ = running.child.kill();
            let _ = running.child.wait();
        }
        self.retry_at = Some(Instant::now() + self.backoff);
        self.backoff = (self.backoff * 2).min(BACKOFF_MAX);
    }

    fn rpc(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.ensure_running()?;
        let program = self.program().to_owned();
        let Some(running) = self.running.as_mut() else {
            return Err(format!("{program} is not running"));
        };
        match request(
            running,
            &mut self.next_id,
            self.timeout,
            &program,
            method,
            params,
        ) {
            Ok(value) => Ok(value),
            Err(RpcFailure::Provider(message)) => Err(message),
            Err(RpcFailure::Broken(message)) => {
                self.fail();
                Err(message)
            }
        }
    }
}

enum RpcFailure {
    /// The provider answered with an error. It is still healthy.
    Provider(String),
    /// The provider died, closed its output or did not answer.
    Broken(String),
}

impl From<RpcFailure> for String {
    fn from(f: RpcFailure) -> Self {
        match f {
            RpcFailure::Provider(m) | RpcFailure::Broken(m) => m,
        }
    }
}

fn request(
    running: &mut Running,
    next_id: &mut u64,
    timeout: Duration,
    program: &str,
    method: &str,
    params: Value,
) -> Result<Value, RpcFailure> {
    let id = *next_id;
    *next_id += 1;
    write_message(&mut running.stdin, &Request::new(id, method, params))
        .map_err(|e| RpcFailure::Broken(format!("{program} stopped reading: {e}")))?;
    let deadline = Instant::now() + timeout;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let line = match running.lines.recv_timeout(left) {
            Ok(line) => line,
            Err(RecvTimeoutError::Timeout) => {
                return Err(RpcFailure::Broken(format!(
                    "{program} did not answer {method} within {}s",
                    timeout.as_secs().max(1)
                )))
            }
            Err(RecvTimeoutError::Disconnected) => {
                let status = running.child.wait().ok().and_then(|s| s.code());
                return Err(RpcFailure::Broken(match status {
                    Some(code) => format!("{program} exited with status {code}"),
                    None => format!("{program} exited"),
                }));
            }
        };
        match serde_json::from_str::<FromProvider>(&line) {
            Ok(FromProvider::Response(r)) if r.id == id => {
                return r.into_result().map_err(|e| RpcFailure::Provider(e.message));
            }
            // A notification, or a stale answer to a request that timed out:
            // neither is this answer. `tickets/changed` is honoured by the
            // next poll; v1 does not wake the scheduler for it.
            Ok(_) => continue,
            Err(e) => {
                return Err(RpcFailure::Broken(format!(
                    "{program} wrote something that is not JSON-RPC: {e}"
                )))
            }
        }
    }
}

impl Source for ProviderSource {
    fn fetch(&mut self) -> Result<Value, String> {
        let value = self.rpc(method::ASSIGNED, json!({}))?;
        let assigned: AssignedResult = serde_json::from_value(value).map_err(|e| {
            format!(
                "{} answered tickets/assigned in a shape zax does not know: {e}",
                self.program()
            )
        })?;
        let init = self
            .running
            .as_ref()
            .map(|r| r.init.clone())
            .ok_or_else(|| format!("{} is not running", self.program()))?;
        serde_json::to_value(TicketsTopic {
            name: init.name,
            transitions: init.capabilities.transitions,
            tickets: assigned.tickets,
        })
        .map_err(|e| e.to_string())
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.rpc(method, params)
    }

    fn interval(&self) -> Duration {
        self.poll
    }
}

impl Drop for ProviderSource {
    fn drop(&mut self) {
        if let Some(running) = self.running.take() {
            let Running {
                mut child,
                mut stdin,
                ..
            } = running;
            let _ = write_message(&mut stdin, &Request::new(0, method::SHUTDOWN, json!({})));
            drop(stdin);
            // Give it a moment to exit cleanly, then make sure. Only the
            // direct child is killed; a grandchild is not waited on.
            thread::sleep(Duration::from_millis(50));
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// For `zax doctor`: start the provider, initialize it, and say what it is.
pub fn probe(argv: Vec<String>) -> Result<String, String> {
    let mut source = ProviderSource::new(argv);
    source.ensure_running()?;
    let init = source
        .running
        .as_ref()
        .map(|r| r.init.clone())
        .ok_or("provider did not start")?;
    Ok(format!("{} (protocol {})", init.name, init.protocol))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use std::time::Duration;

    use super::*;

    fn fake(mode: &str) -> Vec<String> {
        let script = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/fake-provider.sh"
        );
        vec![
            "env".into(),
            format!("FAKE_MODE={mode}"),
            "sh".into(),
            script.into(),
        ]
    }

    #[test]
    fn fetch_publishes_the_tickets_topic() {
        let mut source = ProviderSource::new(fake("ok"));
        let value = source.fetch().unwrap();
        let topic: TicketsTopic = serde_json::from_value(value).unwrap();
        assert_eq!(topic.name, "Fake");
        assert!(topic.transitions);
        assert_eq!(topic.tickets[0].key, "FAKE-1");
        assert_eq!(
            source.interval(),
            Duration::from_secs(60),
            "the provider's advertised poll"
        );
    }

    #[test]
    fn calls_pass_through_to_the_provider() {
        let mut source = ProviderSource::new(fake("ok"));
        let value = source
            .call("tickets/transitions", serde_json::json!({"key": "FAKE-1"}))
            .unwrap();
        assert_eq!(value["transitions"][0]["id"], "Done");
    }

    #[test]
    fn a_missing_provider_reaches_subscribers_as_an_error() {
        let mut source = ProviderSource::new(vec!["zax-definitely-not-a-provider".into()]);
        let err = source.fetch().unwrap_err();
        assert!(err.contains("zax-definitely-not-a-provider"), "{err}");
        assert!(
            err.contains("not installed") || err.contains("PATH"),
            "{err}"
        );
    }

    #[test]
    fn a_provider_speaking_another_protocol_is_refused_by_name() {
        let err = ProviderSource::new(fake("badproto")).fetch().unwrap_err();
        assert!(
            err.contains("Fake speaks protocol 2, zax speaks 1"),
            "{err}"
        );
    }

    #[test]
    fn a_provider_that_does_not_answer_times_out() {
        let mut source = ProviderSource::new(fake("hang")).with_timeout(Duration::from_millis(300));
        let err = source.fetch().unwrap_err();
        assert!(err.contains("did not answer"), "{err}");
    }

    #[test]
    fn a_provider_that_died_is_restarted_on_the_next_fetch_after_backoff() {
        let mut source = ProviderSource::new(fake("die"));
        assert!(source.fetch().is_err());
        // Inside the backoff window the error is immediate and says so.
        let err = source.fetch().unwrap_err();
        assert!(err.contains("restarting"), "{err}");
    }

    #[test]
    fn probe_names_the_provider_and_its_protocol() {
        assert_eq!(probe(fake("ok")).unwrap(), "Fake (protocol 1)");
    }
}
