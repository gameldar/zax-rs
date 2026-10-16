//! `zax pane tickets`: the tickets pane, fed by the broker.
//!
//! Unlike the other panes this one fetches nothing itself. It subscribes to
//! the `tickets` and `work` sources the broker shares between every pane,
//! and performs its row actions either through the broker (transitions) or
//! by running zax and the worktree opener directly.

use std::path::Path;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{anyhow, Result};
use ratatui::crossterm::event::{self, Event as TermEvent, KeyEventKind};
use serde_json::json;
use zax_app::WorktreeOpener;
use zax_broker::client::{binary_identity, Client, Event};
use zax_broker::topics::{
    decode_tickets, decode_transitions, decode_work, fetched_at, TicketsSpec, WorkSpec, TICKETS,
    WORK,
};
use zax_broker::BrokerPaths;
use zax_domain::TicketKey;
use zax_exec::{Exec, SystemExec};
use zax_protocol::{method, Snapshot, SourceKey};
use zax_tui::{PaneAction, TicketsPane};

use crate::screen::Screen;
use crate::wiring;

/// How long the loop waits for a keystroke before re-checking the broker.
const TICK: Duration = Duration::from_millis(250);
/// A provider call can wait on Jira, and the source's worker runs one thing
/// at a time, so a call can queue behind a fetch already in flight: up to
/// 30 s for that fetch plus 30 s for the call, each the provider's own
/// timeout. Past that margin the broker itself has stopped answering.
const CALL_TIMEOUT: Duration = Duration::from_secs(65);
/// A failed connect blocks for a few seconds while it waits for a spawned
/// broker, so it is retried on a timer rather than on every tick.
const RECONNECT_EVERY: Duration = Duration::from_secs(5);

pub fn run() -> Result<()> {
    let ctx = wiring::resolve()?;
    let Some(provider) = ctx.config.tickets.provider.clone() else {
        return idle(TicketsPane::unconfigured());
    };
    let main_worktree = {
        use zax_app::GitRepo;
        ctx.git.main_worktree()?
    };
    let home = wiring::home().ok();
    let provider_argv = anchor_provider(provider.argv(), &main_worktree, home.as_deref());
    let provider_name = provider_label(&provider_argv);
    let zax = wiring::zax_bin()?;
    let zax_program = zax.to_string_lossy().into_owned();
    let paths = BrokerPaths::resolve().map_err(|e| anyhow!(e))?;
    let spawn = vec![zax_program.clone(), "broker".to_owned()];
    let identity = binary_identity(&zax);

    let tickets_key = SourceKey::new(
        TICKETS,
        &TicketsSpec {
            provider: provider_argv,
        },
    );
    let work_key = SourceKey::new(
        WORK,
        &WorkSpec {
            worktree_root: ctx.config.worktree.root.as_path().display().to_string(),
        },
    );
    let opener = wiring::opener(&ctx.config);

    let mut pane = TicketsPane::new();
    pane.set_focused(true);
    let mut screen = Screen::enter()?;
    let mut client: Option<Client> = None;
    let mut next_connect = Instant::now();
    // At most one `zax worktree new` at a time: the branch it is making, and
    // where its result arrives.
    let mut starting: Option<(String, Receiver<Option<String>>)> = None;

    // The first connect can block while a broker is spawned; the pane is
    // drawn before it so it is never blank.
    draw(&mut screen, &pane)?;

    loop {
        if client.is_none() && Instant::now() >= next_connect {
            let connected = Client::connect_or_spawn(&paths, identity, &spawn);
            // Timed from when the attempt ended, not when it began: a failed
            // attempt blocks for seconds, and counting that time would leave
            // the pane frozen for most of every interval.
            next_connect = Instant::now() + RECONNECT_EVERY;
            match connected {
                Ok(c) => {
                    let subscribed = c
                        .subscribe(tickets_key.clone())
                        .and_then(|()| c.subscribe(work_key.clone()));
                    match subscribed {
                        Ok(()) => client = Some(c),
                        Err(e) => pane.apply_tickets(None, None, Some(e)),
                    }
                }
                Err(e) => pane.apply_tickets(None, None, Some(e)),
            }
        }

        if let Some(c) = &client {
            let mut lost = None;
            while let Ok(ev) = c.events().try_recv() {
                match ev {
                    Event::Snapshot(s) if s.key == tickets_key => apply_tickets(&mut pane, s),
                    Event::Snapshot(s) if s.key == work_key => apply_work(&mut pane, s),
                    Event::Snapshot(_) => {}
                    Event::Disconnected(why) => lost = Some(why),
                }
            }
            if let Some(why) = lost {
                // The next tick reconnects, spawning a broker if none is left;
                // until then the pane keeps its last rows, marked stale.
                pane.apply_tickets(None, None, Some(format!("broker: {why}; reconnecting")));
                client = None;
            }
        }

        if let Some((_, done)) = &starting {
            match done.try_recv() {
                Ok(Some(message)) => {
                    pane.notice(message);
                    starting = None;
                }
                Ok(None) => {
                    if let Some(c) = &client {
                        // The new worktree should show on its row now, not at
                        // the next minute's poll.
                        let _ = c.refresh(work_key.clone());
                    }
                    starting = None;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    pane.notice("worktree new: the start was lost".to_owned());
                    starting = None;
                }
            }
        }

        draw(&mut screen, &pane)?;

        if !event::poll(TICK)? {
            continue;
        }
        match event::read()? {
            TermEvent::Key(key) if key.kind == KeyEventKind::Press => match pane.on_key(key) {
                PaneAction::Quit => break,
                PaneAction::Refresh => {
                    if let Some(c) = &client {
                        let _ = c.refresh(tickets_key.clone());
                        let _ = c.refresh(work_key.clone());
                    }
                }
                // The two provider calls stay synchronous, since the picker
                // needs the answer before anything else makes sense, but the
                // pane says what it is waiting for before it waits.
                PaneAction::ListTransitions { key } => match &client {
                    Some(c) => {
                        pane.notice(format!("asking {provider_name} for transitions..."));
                        draw(&mut screen, &pane)?;
                        match c
                            .call(
                                tickets_key.clone(),
                                method::TRANSITIONS,
                                json!({ "key": key.as_str() }),
                                CALL_TIMEOUT,
                            )
                            .and_then(|v| decode_transitions(&v))
                        {
                            Ok(transitions) => pane.show_picker(key, transitions),
                            Err(e) => pane.notice(format!("transitions: {e}")),
                        }
                    }
                    None => pane.notice("not connected to the broker".to_owned()),
                },
                // A transition that worked needs no refresh here: the broker
                // refetches a source after every successful call.
                PaneAction::Transition { key, id } => match &client {
                    Some(c) => {
                        pane.notice(format!("moving {key}..."));
                        draw(&mut screen, &pane)?;
                        match c.call(
                            tickets_key.clone(),
                            method::TRANSITION,
                            json!({ "key": key.as_str(), "id": id }),
                            CALL_TIMEOUT,
                        ) {
                            Ok(_) => pane.notice(format!("moved {key}")),
                            Err(e) => pane.notice(format!("transition: {e}")),
                        }
                    }
                    None => pane.notice("not connected to the broker".to_owned()),
                },
                // Off the UI thread: `worktree new` runs the repository's
                // setup hooks, which can take minutes.
                PaneAction::StartWork { branch } => match &starting {
                    Some((already, _)) => pane.notice(format!("already starting {already}")),
                    None => {
                        let branch = branch.as_str().to_owned();
                        pane.notice(format!("starting {branch}..."));
                        let done =
                            spawn_start_work(SystemExec, zax_program.clone(), branch.clone());
                        starting = Some((branch, done));
                    }
                },
                PaneAction::OpenWorktree { path, others } => match opener.open_worktree(&path) {
                    Err(e) => pane.notice(format!("open: {e}")),
                    Ok(()) if others > 0 => {
                        pane.notice(format!("opened {}; {others} more", path.display()));
                    }
                    Ok(()) => {}
                },
                // A tmux popup over the whole window, run to the end: the
                // pane has nothing to draw while it is open. Whatever was
                // removed shows on the rows at once, not at the next poll.
                PaneAction::ListWorktrees { key } => {
                    if let Some(message) = worktrees_popup(&SystemExec, &zax_program, &key) {
                        pane.notice(message);
                    }
                    if let Some(c) = &client {
                        let _ = c.refresh(work_key.clone());
                    }
                }
                other => {
                    if let Some(message) = crate::cmd_pane::perform(other)? {
                        pane.notice(message);
                    }
                }
            },
            // As in the other panes: refresh on focus, and clear the flag
            // again on the way out so only one pane draws as focused.
            TermEvent::FocusGained => {
                pane.set_focused(true);
                if let Some(c) = &client {
                    let _ = c.refresh(tickets_key.clone());
                }
            }
            TermEvent::FocusLost => pane.set_focused(false),
            _ => {}
        }
    }
    Ok(())
}

fn draw(screen: &mut Screen, pane: &TicketsPane) -> Result<()> {
    screen
        .terminal
        .draw(|f| pane.render(f, f.area(), SystemTime::now()))?;
    Ok(())
}

/// The provider argv as the broker should run it.
///
/// The broker is shared by every repository and inherits nothing from the
/// pane that happened to start it, so a relative path in the program would
/// resolve against the wrong directory, and two repositories configuring
/// the same relative path would share one source. A path is anchored at
/// the repository's main worktree, which is where the layer that names it
/// lives. A leading `~/` is the user's home, as a shell would read it, so
/// it is expanded first (and left as written when there is no home, where
/// anchoring it would only hide the mistake). A bare name is a PATH lookup,
/// which means the same thing everywhere, so it is left alone.
pub(crate) fn anchor_provider(
    argv: &[String],
    main_worktree: &Path,
    home: Option<&Path>,
) -> Vec<String> {
    let mut argv = argv.to_vec();
    if let Some(program) = argv.first_mut() {
        if let Some(rest) = program.strip_prefix("~/") {
            if let Some(home) = home {
                *program = home.join(rest).to_string_lossy().into_owned();
            }
        } else if program.contains('/') && Path::new(program.as_str()).is_relative() {
            *program = main_worktree.join(&*program).to_string_lossy().into_owned();
        }
    }
    argv
}

/// What the pane calls the provider while it waits on it: the program's
/// file name, which is enough to tell `zax-jira` from anything else.
fn provider_label(argv: &[String]) -> String {
    argv.first()
        .and_then(|p| Path::new(p).file_name())
        .map_or_else(
            || "the provider".to_owned(),
            |n| n.to_string_lossy().into_owned(),
        )
}

/// Run `start_work` on its own thread; the result arrives on the receiver.
fn spawn_start_work<E: Exec + Send + 'static>(
    exec: E,
    zax: String,
    branch: String,
) -> Receiver<Option<String>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(start_work(&exec, &zax, &branch));
    });
    rx
}

fn apply_tickets(pane: &mut TicketsPane, snapshot: Snapshot) {
    let at = snapshot.fetched_at.map(fetched_at);
    match snapshot.value.as_ref().map(decode_tickets).transpose() {
        Ok(list) => pane.apply_tickets(
            list.map(|l| (l.provider, l.transitions, l.tickets)),
            at,
            snapshot.error,
        ),
        Err(e) => pane.apply_tickets(None, None, Some(e)),
    }
}

fn apply_work(pane: &mut TicketsPane, snapshot: Snapshot) {
    let at = snapshot.fetched_at.map(fetched_at);
    match snapshot.value.as_ref().map(decode_work).transpose() {
        Ok(work) => pane.apply_work(work, at, snapshot.error),
        Err(e) => pane.apply_work(None, None, Some(e)),
    }
}

/// The unconfigured pane: draw, wait for `q`.
fn idle(mut pane: TicketsPane) -> Result<()> {
    let mut screen = Screen::enter()?;
    loop {
        screen
            .terminal
            .draw(|f| pane.render(f, f.area(), SystemTime::now()))?;
        if event::poll(TICK)? {
            if let TermEvent::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press && pane.on_key(key) == PaneAction::Quit {
                    return Ok(());
                }
            }
        }
    }
}

/// `zax worktree new <branch>` in the current repository; the message the
/// pane shows when it did not work.
///
/// zax prints its whole error chain as the last line of stderr, so that line
/// is the message, as it is for `zax worktree review` in the PR panes.
fn start_work<E: Exec>(exec: &E, zax: &str, branch: &str) -> Option<String> {
    let result = exec.exec(
        zax,
        &["worktree".to_owned(), "new".to_owned(), branch.to_owned()],
        None,
        &[],
    );
    let detail = match result {
        Ok(out) if out.ok() => return None,
        Ok(out) => out
            .stderr
            .lines()
            .rev()
            .map(str::trim)
            .map(|line| line.strip_prefix("zax: ").unwrap_or(line))
            .find(|line| !line.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("exited with status {}", out.status)),
        Err(error) => error.to_string(),
    };
    Some(format!("worktree new: {detail}"))
}

/// The popup's own tmux command, run by the pane. `-E` closes the popup when
/// the command exits and makes this call wait for that, and `-w`/`-h` of 100%
/// cover every pane rather than just this one. `-d` is this pane's directory,
/// because `zax` resolves the repository from where it runs and a popup
/// otherwise starts wherever the tmux server did. Returns what the pane
/// should say when the popup could not be opened.
fn worktrees_popup<E: Exec>(exec: &E, zax: &str, key: &TicketKey) -> Option<String> {
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(e) => return Some(format!("worktrees: {e}")),
    };
    let args = [
        "display-popup".to_owned(),
        "-E".to_owned(),
        "-b".to_owned(),
        "rounded".to_owned(),
        "-T".to_owned(),
        format!(" worktrees {key} "),
        "-w".to_owned(),
        "100%".to_owned(),
        "-h".to_owned(),
        "100%".to_owned(),
        "-d".to_owned(),
        cwd.to_string_lossy().into_owned(),
        zax.to_owned(),
        "pane".to_owned(),
        "worktrees".to_owned(),
        key.to_string(),
    ];
    match exec.exec("tmux", &args, None, &[]) {
        Ok(out) if out.ok() => None,
        Ok(out) => Some(format!(
            "worktrees: {}",
            out.stderr
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .map_or_else(
                    || format!("tmux exited with status {}", out.status),
                    str::to_owned
                )
        )),
        Err(e) => Some(format!("worktrees: {e}")),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use std::path::Path;
    use std::time::Duration;

    use zax_exec::{Exec, ExecError, Output, RecordingExec};

    use super::{anchor_provider, spawn_start_work, start_work, worktrees_popup};

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|p| (*p).to_owned()).collect()
    }

    #[test]
    fn a_relative_provider_path_is_anchored_at_the_main_worktree() {
        // The broker is shared by every repository and started from
        // whichever pane got there first, so a relative path would resolve
        // against the wrong directory and two repos' providers would alias.
        let anchored = anchor_provider(
            &argv(&["./target/debug/zax-jira", "--verbose"]),
            Path::new("/src/omega"),
            None,
        );
        assert_eq!(
            anchored,
            argv(&["/src/omega/./target/debug/zax-jira", "--verbose"])
        );
        assert_eq!(
            anchor_provider(&argv(&["bin/jira"]), Path::new("/src/omega"), None),
            argv(&["/src/omega/bin/jira"])
        );
    }

    #[test]
    fn a_home_relative_provider_path_is_expanded_not_anchored() {
        let home = Path::new("/home/me");
        assert_eq!(
            anchor_provider(
                &argv(&["~/bin/zax-jira", "-v"]),
                Path::new("/src/omega"),
                Some(home)
            ),
            argv(&["/home/me/bin/zax-jira", "-v"])
        );
        // Without a home there is nothing to expand it to, and anchoring
        // it at the repository would only hide what went wrong.
        assert_eq!(
            anchor_provider(&argv(&["~/bin/zax-jira"]), Path::new("/src/omega"), None),
            argv(&["~/bin/zax-jira"])
        );
    }

    #[test]
    fn a_bare_or_absolute_provider_is_left_alone() {
        // A bare name is a PATH lookup, which means the same thing anywhere.
        for given in [argv(&["zax-jira", "x"]), argv(&["/usr/bin/zax-jira"])] {
            assert_eq!(
                anchor_provider(&given, Path::new("/src/omega"), None),
                given
            );
        }
        assert!(anchor_provider(&[], Path::new("/src/omega"), None).is_empty());
    }

    /// `RecordingExec` is not `Send`; the background start needs an exec
    /// that can cross to its thread.
    struct Refusing;

    impl Exec for Refusing {
        fn exec(
            &self,
            _program: &str,
            _args: &[String],
            _cwd: Option<&Path>,
            _env: &[zax_exec::EnvVar],
        ) -> Result<Output, ExecError> {
            Ok(Output {
                status: 1,
                stdout: String::new(),
                stderr: "zax: branch exists\n".into(),
            })
        }
    }

    #[test]
    fn a_background_start_posts_its_result_back() {
        let done = spawn_start_work(Refusing, "zax".to_owned(), "OMEGA-1-go".to_owned());
        let message = done.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(message.as_deref(), Some("worktree new: branch exists"));
    }

    #[test]
    fn start_work_runs_worktree_new_with_the_branch() {
        let exec = RecordingExec::returning("");
        assert_eq!(start_work(&exec, "zax", "OMEGA-1-go"), None);
        assert_eq!(exec.args(0), ["worktree", "new", "OMEGA-1-go"]);
    }

    #[test]
    fn a_refused_start_reaches_the_pane() {
        let exec = RecordingExec::with_responses(vec![Output {
            status: 1,
            stdout: String::new(),
            stderr: "zax: branch exists\n".into(),
        }]);
        assert_eq!(
            start_work(&exec, "zax", "OMEGA-1-go").unwrap(),
            "worktree new: branch exists"
        );
    }

    #[test]
    fn a_start_that_could_not_run_zax_is_reported_too() {
        // With no response queued the recorder fails the exec itself, which
        // is what a missing or unrunnable binary looks like.
        let exec = RecordingExec::with_responses(Vec::new());
        let message = start_work(&exec, "zax", "OMEGA-1-go").unwrap();
        assert!(message.starts_with("worktree new: "), "got: {message}");
        assert!(message.contains("zax"), "got: {message}");
    }

    #[test]
    fn a_failure_with_nothing_to_say_still_says_something() {
        let exec = RecordingExec::with_responses(vec![Output {
            status: 2,
            stdout: String::new(),
            stderr: "  \n".into(),
        }]);
        let message = start_work(&exec, "zax", "OMEGA-1-go").unwrap();
        assert!(message.contains('2'), "got: {message}");
    }

    #[test]
    fn the_worktrees_popup_covers_the_window_and_runs_this_zax() {
        let exec = RecordingExec::returning("");
        let key = zax_domain::TicketKey::parse("OMEGA-1").unwrap();
        assert_eq!(worktrees_popup(&exec, "/bin/zax", &key), None);
        let args = exec.args(0);
        assert_eq!(args[0], "display-popup");
        assert!(args.contains(&"-E".to_owned()));
        let at = |flag: &str| args.iter().position(|a| a == flag).unwrap();
        assert_eq!(args[at("-w") + 1], "100%");
        assert_eq!(args[at("-h") + 1], "100%");
        assert_eq!(
            args[args.len() - 4..],
            ["/bin/zax", "pane", "worktrees", "OMEGA-1"]
        );
        assert_eq!(exec.calls()[0].program, "tmux");
    }

    #[test]
    fn a_popup_tmux_would_not_open_is_reported() {
        let exec = RecordingExec::with_responses(vec![Output {
            status: 1,
            stdout: String::new(),
            stderr: "no server running\n".into(),
        }]);
        let key = zax_domain::TicketKey::parse("OMEGA-1").unwrap();
        assert_eq!(
            worktrees_popup(&exec, "/bin/zax", &key).unwrap(),
            "worktrees: no server running"
        );
    }
}
