//! A ticket provider as a broker source: a child process speaking JSON-RPC
//! over its stdin and stdout.
//!
//! The child is started on first use and kept. A child that exits, closes
//! its output or does not answer in time is killed, and the next start waits
//! out an exponential backoff, so a broken provider is retried without being
//! hammered. A provider that answers `initialize` with an error is healthy
//! but refusing (logged out, say), so it is reaped without growing the
//! backoff and is tried again as soon as anyone asks. The last good value is
//! the hub's to keep; this only reports.

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
/// The soonest a failing provider asks to be fetched again, so a backoff
/// that is about to end does not turn into a busy loop.
const RETRY_FLOOR: Duration = Duration::from_secs(1);

struct Running {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    init: InitializeResult,
}

impl Drop for Running {
    /// Any early return that abandons a child also reaps it.
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub struct ProviderSource {
    argv: Vec<String>,
    timeout: Duration,
    running: Option<Running>,
    next_id: u64,
    backoff: Duration,
    retry_at: Option<Instant>,
    /// What last broke the provider, repeated while the backoff runs so the
    /// pane keeps saying what is wrong rather than only that something is.
    last_error: Option<String>,
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
            last_error: None,
            poll: DEFAULT_POLL,
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// While failing, the next fetch is due when the backoff ends rather
    /// than a whole poll later, so restarts follow the backoff schedule.
    fn interval_at(&self, now: Instant) -> Duration {
        match self.retry_at {
            Some(at) => at
                .saturating_duration_since(now)
                .min(self.poll)
                .max(RETRY_FLOOR),
            None => self.poll,
        }
    }

    fn program(&self) -> &str {
        self.argv.first().map(String::as_str).unwrap_or("provider")
    }

    fn ensure_running(&mut self) -> Result<(), String> {
        if self.running.is_some() {
            return Ok(());
        }
        if let Some(at) = self.retry_at {
            let now = Instant::now();
            if now < at {
                let last = self
                    .last_error
                    .clone()
                    .unwrap_or_else(|| format!("{} failed", self.program()));
                return Err(format!("{last} (retrying in {}s)", ceil_secs(at - now)));
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
                Ok(())
            }
            Err(RpcFailure::Provider(message)) => {
                // It answered, so it is healthy; what it refused (a login,
                // say) may be put right at any moment, and the next try
                // must not wait out a backoff for it. Dropping the result
                // has already reaped the child.
                self.retry_at = None;
                Err(message)
            }
            Err(RpcFailure::Broken(message)) => {
                self.fail(&message);
                Err(message)
            }
        }
    }

    fn start(&mut self) -> Result<Running, RpcFailure> {
        let (program, args) = self
            .argv
            .split_first()
            .ok_or_else(|| RpcFailure::Broken("no provider command".into()))?;
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| {
                RpcFailure::Broken(if e.kind() == std::io::ErrorKind::NotFound {
                    format!("{program} is not installed or not on PATH")
                } else {
                    format!("could not start {program}: {e}")
                })
            })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| RpcFailure::Broken("provider has no stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| RpcFailure::Broken("provider has no stdout".into()))?;
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
        .map_err(|e| RpcFailure::Broken(e.to_string()))?;
        let value = request(
            &mut running,
            &mut self.next_id,
            self.timeout,
            program,
            method::INITIALIZE,
            params,
        )?;
        let init: InitializeResult = serde_json::from_value(value).map_err(|e| {
            RpcFailure::Broken(format!(
                "{program} answered initialize in a shape zax does not know: {e}"
            ))
        })?;
        if init.protocol != PROTOCOL_VERSION {
            return Err(RpcFailure::Broken(format!(
                "{} speaks protocol {}, zax speaks {PROTOCOL_VERSION}",
                init.name, init.protocol
            )));
        }
        running.init = init;
        Ok(running)
    }

    /// Kill the child and schedule the next start.
    fn fail(&mut self, message: &str) {
        self.running = None;
        self.last_error = Some(message.to_owned());
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
            Ok(value) => {
                // Only a provider that answers a request has earned a fresh
                // backoff; one that initializes and then dies keeps growing it.
                self.backoff = BACKOFF_MIN;
                self.retry_at = None;
                self.last_error = None;
                Ok(value)
            }
            Err(RpcFailure::Provider(message)) => Err(message),
            Err(RpcFailure::Broken(message)) => {
                self.fail(&message);
                Err(message)
            }
        }
    }
}

/// Whole seconds, rounded up, so a wait under a second still reads as 1s.
fn ceil_secs(d: Duration) -> u64 {
    d.as_secs() + u64::from(d.subsec_nanos() > 0)
}

enum RpcFailure {
    /// The provider answered with an error. It is still healthy.
    Provider(String),
    /// The provider died, closed its output or did not answer.
    Broken(String),
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
        // Chatter that is not our answer must not stretch the deadline.
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(RpcFailure::Broken(format!(
                "{program} did not answer {method} within {}s",
                timeout.as_secs().max(1)
            )));
        }
        let line = match running.lines.recv_timeout(left) {
            Ok(line) => line,
            Err(RecvTimeoutError::Timeout) => {
                return Err(RpcFailure::Broken(format!(
                    "{program} did not answer {method} within {}s",
                    timeout.as_secs().max(1)
                )))
            }
            Err(RecvTimeoutError::Disconnected) => {
                // The reader also ends when stdout closes or is unreadable
                // while the child lives on, so never wait on a live child.
                let status = match running.child.try_wait().ok().flatten() {
                    Some(s) => s.code(),
                    None => {
                        let _ = running.child.kill();
                        running.child.wait().ok().and_then(|s| s.code())
                    }
                };
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
        self.interval_at(Instant::now())
    }
}

impl Drop for ProviderSource {
    fn drop(&mut self) {
        if let Some(mut running) = self.running.take() {
            let _ = write_message(
                &mut running.stdin,
                &Request::new(0, method::SHUTDOWN, json!({})),
            );
            // Give it a moment to exit cleanly; dropping `running` then
            // makes sure. Only the direct child is killed; a grandchild is
            // not waited on.
            thread::sleep(Duration::from_millis(50));
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
    fn inside_the_backoff_window_the_last_real_error_is_repeated() {
        let mut source = ProviderSource::new(fake("die"));
        // Whether the write or the read notices the exit first is a race,
        // so the wording of the first error is not pinned.
        let first = source.fetch().unwrap_err();
        // Inside the backoff window the error is immediate, and still says
        // what went wrong rather than only that something did. The window
        // is stretched so a loaded machine cannot outlast it.
        source.retry_at = Some(Instant::now() + Duration::from_secs(30));
        let err = source.fetch().unwrap_err();
        assert_eq!(err, format!("{first} (retrying in 30s)"));
    }

    #[test]
    fn an_error_answer_to_initialize_does_not_grow_the_backoff() {
        let mut source = ProviderSource::new(fake("noauth"));
        let err = source.fetch().unwrap_err();
        assert!(err.contains("acli jira auth login"), "{err}");
        assert!(source.running.is_none(), "the child is reaped");
        assert_eq!(source.backoff, BACKOFF_MIN, "it answered, so it is healthy");
        assert_eq!(source.retry_at, None);
        // So a retry after logging in starts it again at once.
        let again = source.fetch().unwrap_err();
        assert!(!again.contains("retrying"), "{again}");
        assert!(again.contains("acli jira auth login"), "{again}");
    }

    #[test]
    fn while_failing_the_interval_follows_the_backoff() {
        let mut source = ProviderSource::new(fake("die"));
        assert!(source.fetch().is_err());
        assert_eq!(
            source.interval(),
            Duration::from_secs(1),
            "the restart is due when the backoff ends, not at the next poll"
        );
        let now = Instant::now();
        source.retry_at = Some(now + Duration::from_secs(30));
        assert_eq!(source.interval_at(now), Duration::from_secs(30));
        source.retry_at = Some(now + Duration::from_secs(600));
        assert_eq!(
            source.interval_at(now),
            Duration::from_secs(60),
            "never later than the provider's own poll"
        );
        source.retry_at = Some(now + Duration::from_millis(10));
        assert_eq!(source.interval_at(now), Duration::from_secs(1), "floored");
    }

    #[test]
    fn a_provider_that_closes_stdout_but_lives_is_an_error_not_a_hang() {
        let mut source = ProviderSource::new(fake("closeout")).with_timeout(Duration::from_secs(5));
        let started = Instant::now();
        let err = source.fetch().unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(4), "{err}");
        assert!(err.contains("exited"), "{err}");
    }

    #[test]
    fn a_provider_that_dies_on_its_first_request_keeps_backing_off() {
        let mut source = ProviderSource::new(fake("die"));
        assert!(source.fetch().is_err());
        assert_eq!(
            source.backoff,
            BACKOFF_MIN * 2,
            "initialize alone must not reset the backoff"
        );
    }

    #[test]
    fn probe_names_the_provider_and_its_protocol() {
        assert_eq!(probe(fake("ok")).unwrap(), "Fake (protocol 1)");
    }
}
