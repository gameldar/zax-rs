//! `zax pane todo`: the todo pane, fed by the broker.
//!
//! The broker owns the file. This loop subscribes to it, sends every edit
//! as a broker call, and tells the pane what today is.

use std::time::{Duration, Instant, SystemTime};

use anyhow::{anyhow, Result};
use ratatui::crossterm::event::{self, Event as TermEvent, KeyEventKind};
use zax_app::Editor;
use zax_broker::client::{binary_identity, Client, Event};
use zax_broker::topics::{decode_todo, encode_todo_edit, fetched_at, TodoSpec, TODO};
use zax_broker::BrokerPaths;
use zax_domain::keymap::PaneContext;
use zax_domain::{Date, TodoDocument, TodoFile};
use zax_protocol::{Snapshot, SourceKey};
use zax_tui::{PaneAction, TodoPane};

use crate::screen::Screen;
use crate::wiring;

/// How long the loop waits for a keystroke before re-checking the broker.
const TICK: Duration = Duration::from_millis(250);
/// A todo write is a local file, but it can queue behind a fetch on the
/// source's one worker. Past this the broker has stopped answering.
const CALL_TIMEOUT: Duration = Duration::from_secs(10);
/// A failed connect blocks for a few seconds while it waits for a spawned
/// broker, so it is retried on a timer rather than on every tick.
const RECONNECT_EVERY: Duration = Duration::from_secs(5);
/// How often the pane is told the date, so it rolls over at midnight.
const TODAY_EVERY: Duration = Duration::from_secs(60);

pub fn run() -> Result<()> {
    let ctx = wiring::resolve()?;
    let file = wiring::todo_file(&ctx.config)?;
    let file_text = file.as_path().display().to_string();
    let zax = wiring::zax_bin()?;
    let paths = BrokerPaths::resolve().map_err(|e| anyhow!(e))?;
    let spawn = vec![zax.to_string_lossy().into_owned(), "broker".to_owned()];
    let identity = binary_identity(&zax);
    let key = SourceKey::new(
        TODO,
        &TodoSpec {
            file: file_text.clone(),
        },
    );
    let editor = wiring::editor(&ctx.config);

    let mut pane = TodoPane::new(
        file_text,
        local_today()?,
        ctx.config.keymaps.for_context(PaneContext::Todo).clone(),
    );
    pane.set_focused(true);
    let mut screen = Screen::enter()?;
    let mut client: Option<Client> = None;
    let mut next_connect = Instant::now();
    let mut next_today = Instant::now() + TODAY_EVERY;

    draw(&mut screen, &pane)?;

    loop {
        if client.is_none() && Instant::now() >= next_connect {
            let connected = Client::connect_or_spawn(&paths, identity, &spawn);
            next_connect = Instant::now() + RECONNECT_EVERY;
            match connected.and_then(|c| c.subscribe(key.clone()).map(|()| c)) {
                Ok(c) => client = Some(c),
                Err(e) => pane.apply(None, None, Some(e)),
            }
        }

        if let Some(c) = &client {
            let mut lost = None;
            while let Ok(ev) = c.events().try_recv() {
                match ev {
                    Event::Snapshot(s) if s.key == key => apply(&mut pane, s),
                    Event::Snapshot(_) => {}
                    Event::Disconnected(why) => lost = Some(why),
                }
            }
            if let Some(why) = lost {
                pane.apply(None, None, Some(format!("broker: {why}; reconnecting")));
                client = None;
            }
        }

        if Instant::now() >= next_today {
            next_today = Instant::now() + TODAY_EVERY;
            if let Ok(today) = local_today() {
                pane.set_today(today);
            }
        }

        draw(&mut screen, &pane)?;

        if !event::poll(TICK)? {
            continue;
        }
        match event::read()? {
            TermEvent::Key(k) if k.kind == KeyEventKind::Press => match pane.on_key(k) {
                PaneAction::Quit => break,
                PaneAction::Refresh => {
                    if let Some(c) = &client {
                        let _ = c.refresh(key.clone());
                    }
                }
                // A write that worked needs no refresh here: the broker
                // refetches a source after every successful call. A write
                // that failed may have failed because the file moved, so the
                // pane is shown the file as it is now.
                PaneAction::Todo(edit) => match &client {
                    Some(c) => {
                        let (method, params) = encode_todo_edit(&edit);
                        if let Err(e) = c.call(key.clone(), method, params, CALL_TIMEOUT) {
                            pane.notice(e);
                            let _ = c.refresh(key.clone());
                        }
                    }
                    None => pane.notice("not connected to the broker".to_owned()),
                },
                PaneAction::OpenFile(path) => {
                    if let Err(e) = editor.edit(&path) {
                        pane.notice(format!("editor: {e}"));
                    }
                }
                other => {
                    if let Some(message) = crate::cmd_pane::perform(other)? {
                        pane.notice(message);
                    }
                }
            },
            TermEvent::FocusGained => {
                pane.set_focused(true);
                if let Some(c) = &client {
                    let _ = c.refresh(key.clone());
                }
            }
            TermEvent::FocusLost => pane.set_focused(false),
            TermEvent::Mouse(mouse) => pane.on_mouse(mouse),
            TermEvent::Paste(text) => pane.on_paste(&text),
            _ => {}
        }
    }
    Ok(())
}

fn draw(screen: &mut Screen, pane: &TodoPane) -> Result<()> {
    screen
        .terminal
        .draw(|f| pane.render(f, f.area(), SystemTime::now()))?;
    Ok(())
}

fn apply(pane: &mut TodoPane, snapshot: Snapshot) {
    let at = snapshot.fetched_at.map(fetched_at);
    match snapshot.value.as_ref().map(decode_todo).transpose() {
        Ok(list) => pane.apply(list.map(|l| l.items), at, snapshot.error),
        Err(e) => pane.apply(None, None, Some(e)),
    }
}

/// Today in the system time zone. `jiff` reads the zone itself, which
/// `time` refuses to do once a process has threads.
pub(crate) fn local_today() -> Result<Date> {
    let now = jiff::Zoned::now();
    Date::from_ymd(
        i32::from(now.year()),
        u32::try_from(now.month())?,
        u32::try_from(now.day())?,
    )
    .map_err(|e| anyhow!(e))
}

/// For `zax doctor`: where the file is and whether it reads.
pub(crate) fn probe(file: &TodoFile) -> Result<String, String> {
    let path = file.as_path();
    match std::fs::read(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Ok(format!("{} (no file yet)", path.display()))
        }
        Err(e) => Err(format!("{}: {e}", path.display())),
        Ok(bytes) => {
            let text = String::from_utf8(bytes)
                .map_err(|_| format!("{} is not UTF-8 text", path.display()))?;
            let open = TodoDocument::parse(&text)
                .items()
                .iter()
                .filter(|i| !i.done)
                .count();
            Ok(format!("{} ({open} open)", path.display()))
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn today_is_a_real_date() {
        let today = local_today().unwrap();
        assert!(today.year() >= 2026, "{today}");
    }

    #[test]
    fn a_missing_file_probes_as_not_created_yet() {
        let dir = tempfile::tempdir().unwrap();
        let file = TodoFile::new(dir.path().join("todo.md")).unwrap();
        assert!(probe(&file).unwrap().contains("no file yet"));
    }

    #[test]
    fn a_file_probes_with_its_open_count() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("todo.md");
        std::fs::write(&path, "- [ ] a\n- [x] b\n- [ ] c\n").unwrap();
        let detail = probe(&TodoFile::new(path).unwrap()).unwrap();
        assert!(detail.contains("2 open"), "{detail}");
    }

    #[test]
    fn an_unreadable_file_fails_the_probe() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("todo.md");
        std::fs::write(&path, [0xff, 0xfe]).unwrap();
        assert!(probe(&TodoFile::new(path).unwrap())
            .unwrap_err()
            .contains("UTF-8"));
    }

    #[test]
    fn a_directory_fails_the_probe() {
        let dir = tempfile::tempdir().unwrap();
        let file = TodoFile::new(PathBuf::from(dir.path())).unwrap();
        assert!(probe(&file).is_err());
    }
}
