//! `zax pane tickets`: the tickets pane, fed by the broker.
//!
//! Unlike the other panes this one fetches nothing itself. It subscribes to
//! the `tickets` and `work` sources the broker shares between every pane,
//! and performs its row actions either through the broker (transitions) or
//! by running zax and the editor directly.

use std::time::{Duration, Instant, SystemTime};

use anyhow::{anyhow, Result};
use ratatui::crossterm::event::{self, Event as TermEvent, KeyEventKind};
use serde_json::json;
use zax_app::Editor;
use zax_broker::client::{binary_identity, Client, Event};
use zax_broker::topics::{
    decode_tickets, decode_transitions, decode_work, fetched_at, TicketsSpec, WorkSpec, TICKETS,
    WORK,
};
use zax_broker::BrokerPaths;
use zax_exec::{Exec, SystemExec};
use zax_protocol::{method, Snapshot, SourceKey};
use zax_tui::{PaneAction, TicketsPane};

use crate::screen::Screen;
use crate::wiring;

/// How long the loop waits for a keystroke before re-checking the broker.
const TICK: Duration = Duration::from_millis(250);
/// A provider call can wait on Jira; the provider's own timeout is 30 s, so
/// this only fires when the broker itself has stopped answering.
const CALL_TIMEOUT: Duration = Duration::from_secs(40);
/// A failed connect blocks for a few seconds while it waits for a spawned
/// broker, so it is retried on a timer rather than on every tick.
const RECONNECT_EVERY: Duration = Duration::from_secs(5);

pub fn run() -> Result<()> {
    let ctx = wiring::resolve()?;
    let Some(provider) = ctx.config.tickets.provider.clone() else {
        return idle(TicketsPane::unconfigured());
    };
    let zax = wiring::zax_bin()?;
    let zax_program = zax.to_string_lossy().into_owned();
    let paths = BrokerPaths::resolve().map_err(|e| anyhow!(e))?;
    let spawn = vec![zax_program.clone(), "broker".to_owned()];
    let identity = binary_identity(&zax);

    let tickets_key = SourceKey::new(
        TICKETS,
        &TicketsSpec {
            provider: provider.argv().to_vec(),
        },
    );
    let work_key = SourceKey::new(
        WORK,
        &WorkSpec {
            worktree_root: ctx.config.worktree.root.as_path().display().to_string(),
        },
    );
    let editor = wiring::editor(&ctx.config);

    let mut pane = TicketsPane::new();
    pane.set_focused(true);
    let mut screen = Screen::enter()?;
    let mut client: Option<Client> = None;
    let mut next_connect = Instant::now();

    loop {
        if client.is_none() && Instant::now() >= next_connect {
            next_connect = Instant::now() + RECONNECT_EVERY;
            match Client::connect_or_spawn(&paths, identity, &spawn) {
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

        screen
            .terminal
            .draw(|f| pane.render(f, f.area(), SystemTime::now()))?;

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
                PaneAction::ListTransitions { key } => match &client {
                    Some(c) => match c
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
                    },
                    None => pane.notice("not connected to the broker".to_owned()),
                },
                // A transition that worked needs no refresh here: the broker
                // refetches a source after every successful call.
                PaneAction::Transition { key, id } => match &client {
                    Some(c) => {
                        if let Err(e) = c.call(
                            tickets_key.clone(),
                            method::TRANSITION,
                            json!({ "key": key.as_str(), "id": id }),
                            CALL_TIMEOUT,
                        ) {
                            pane.notice(format!("transition: {e}"));
                        }
                    }
                    None => pane.notice("not connected to the broker".to_owned()),
                },
                PaneAction::StartWork { branch } => {
                    if let Some(message) = start_work(&SystemExec, &zax_program, branch.as_str()) {
                        pane.notice(message);
                    } else if let Some(c) = &client {
                        // The new worktree should show on its row now, not at
                        // the next minute's poll.
                        let _ = c.refresh(work_key.clone());
                    }
                }
                PaneAction::OpenWorktree { path, others } => match editor.open(&path) {
                    Err(e) => pane.notice(format!("editor: {e}")),
                    Ok(()) if others > 0 => {
                        pane.notice(format!("opened {}; {others} more", path.display()));
                    }
                    Ok(()) => {}
                },
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use zax_exec::{Output, RecordingExec};

    use super::start_work;

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
}
