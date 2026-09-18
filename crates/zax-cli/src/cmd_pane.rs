//! The pane processes: a terminal loop over a `zax-tui` pane.
//!
//! Everything this loop decides is already tested in `zax-tui`. What is left
//! here is wiring: raw mode, a fetch thread, and turning a `PaneAction` into a
//! side effect.

use std::io::stdout;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result};
use ratatui::crossterm::event::{self, DisableFocusChange, EnableFocusChange, Event, KeyEventKind};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::prelude::CrosstermBackend;
use ratatui::Terminal;
use zax_app::queries;
use zax_domain::{parse_duration, Command};
use zax_exec::{Exec, SystemExec};
use zax_tui::{refresh, BranchStatusPane, ExecPane, PaneAction, PrsForReviewPane};

use crate::cli::PaneCommand;
use crate::wiring;

const JITTER: f64 = 0.15;
/// How long the loop waits for a keystroke before re-checking the clock.
const TICK: Duration = Duration::from_millis(250);

pub fn run(command: PaneCommand) -> Result<()> {
    match command {
        PaneCommand::PrsForReview => run_prs_pane(),
        PaneCommand::BranchStatus => run_branch_pane(),
        PaneCommand::Exec {
            title,
            every,
            command,
        } => {
            let every = parse_duration(&every)?;
            let command = Command::new(command)?;
            run_exec_pane(title, every, command)
        }
    }
}

/// A pseudo-random roll in `[0, 1)` without a dependency: the sub-second part of
/// the clock is more than random enough to stop panes polling in lockstep.
fn roll() -> f64 {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    f64::from(nanos) / 1_000_000_000.0
}

/// Runs fetches off the UI thread so a slow `gh` never freezes keystrokes.
fn spawn_fetcher<T, F>(fetch: F) -> (Sender<()>, Receiver<Result<T, String>>)
where
    T: Send + 'static,
    F: Fn() -> Result<T, String> + Send + 'static,
{
    let (request_tx, request_rx) = mpsc::channel::<()>();
    let (result_tx, result_rx) = mpsc::channel::<Result<T, String>>();
    thread::spawn(move || {
        while request_rx.recv().is_ok() {
            if result_tx.send(fetch()).is_err() {
                break;
            }
        }
    });
    (request_tx, result_rx)
}

struct Screen {
    terminal: Terminal<CrosstermBackend<std::io::Stdout>>,
}

impl Screen {
    fn enter() -> Result<Self> {
        enable_raw_mode().context("could not put the terminal into raw mode")?;
        // `EnableFocusChange` is what makes `Event::FocusGained` arrive, which is
        // how a pane refreshes on focus. tmux forwards it because our conf sets
        // `focus-events on`.
        execute!(stdout(), EnterAlternateScreen, EnableFocusChange)?;
        Ok(Self {
            terminal: Terminal::new(CrosstermBackend::new(stdout()))?,
        })
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(stdout(), DisableFocusChange, LeaveAlternateScreen);
    }
}

/// The shape every pane loop shares.
macro_rules! pane_loop {
    ($pane:expr, $every:expr, $request:expr, $results:expr, $on_action:expr) => {{
        let mut pane = $pane;
        pane.set_focused(true);
        let mut screen = Screen::enter()?;
        let mut due = Instant::now();

        $request.send(()).ok();

        loop {
            if Instant::now() >= due {
                $request.send(()).ok();
                due = Instant::now() + refresh::next_delay($every, JITTER, roll());
            }

            while let Ok(result) = $results.try_recv() {
                pane.apply(result, SystemTime::now());
            }

            screen
                .terminal
                .draw(|frame| pane.render(frame, frame.area(), SystemTime::now()))?;

            if event::poll(TICK)? {
                match event::read()? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => match pane.on_key(key) {
                        PaneAction::Quit => break,
                        PaneAction::Refresh => due = Instant::now(),
                        // Refreshing on focus is what the spec asks for.
                        other => $on_action(other)?,
                    },
                    Event::FocusGained => due = Instant::now(),
                    _ => {}
                }
            }
        }
        Ok(())
    }};
}

fn run_prs_pane() -> Result<()> {
    let cwd = std::env::current_dir()?;
    let (request, results) = spawn_fetcher(move || {
        let gh = zax_infra_github::GhCli::new(SystemExec, cwd.clone());
        queries::list_review_requests(&gh).map_err(|e| e.to_string())
    });
    pane_loop!(
        PrsForReviewPane::new(),
        Duration::from_secs(120),
        request,
        results,
        perform
    )
}

fn run_branch_pane() -> Result<()> {
    let cwd = std::env::current_dir()?;
    let (request, results) = spawn_fetcher(move || {
        let git = zax_infra_git::GitCli::new(SystemExec, cwd.clone());
        let gh = zax_infra_github::GhCli::new(SystemExec, cwd.clone());
        queries::branch_status(&git, &gh).map_err(|e| e.to_string())
    });
    pane_loop!(
        BranchStatusPane::new(),
        Duration::from_secs(60),
        request,
        results,
        perform
    )
}

fn run_exec_pane(title: String, every: Duration, command: Command) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let argv: Vec<String> = command.argv().iter().skip(1).cloned().collect();
    let program = command.program().to_owned();
    let (request, results) =
        spawn_fetcher(
            move || match SystemExec.exec(&program, &argv, Some(&cwd), &[]) {
                Ok(out) if out.ok() => Ok(out.stdout),
                Ok(out) => Err(format!("exited {}: {}", out.status, out.stderr.trim())),
                Err(e) => Err(e.to_string()),
            },
        );
    pane_loop!(
        ExecPane::new(title, every),
        every,
        request,
        results,
        perform
    )
}

/// Turn a row action into a side effect. Never fatal: a pane must survive a
/// missing `open` or clipboard tool.
fn perform(action: PaneAction) -> Result<()> {
    match action {
        PaneAction::OpenUrl(url) => {
            let _ = SystemExec.exec(open_program(), &[url], None, &[]);
        }
        PaneAction::CopyUrl(url) => copy_to_clipboard(&url),
        PaneAction::ReviewWorktree { repo, number } => {
            let zax = wiring::zax_bin()?.to_string_lossy().into_owned();
            let _ = SystemExec.exec(
                &zax,
                &[
                    "worktree".to_owned(),
                    "review".to_owned(),
                    format!("https://github.com/{repo}/pull/{number}"),
                ],
                None,
                &[],
            );
        }
        PaneAction::None | PaneAction::Refresh | PaneAction::Quit => {}
    }
    Ok(())
}

fn open_program() -> &'static str {
    if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    }
}

/// Try each clipboard tool in turn. A machine with none is not an error.
fn copy_to_clipboard(text: &str) {
    for program in ["pbcopy", "wl-copy", "xclip"] {
        let args: Vec<String> = if program == "xclip" {
            vec!["-selection".to_owned(), "clipboard".to_owned()]
        } else {
            Vec::new()
        };
        if pipe_into(program, &args, text).is_ok() {
            return;
        }
    }
}

fn pipe_into(program: &str, args: &[String], text: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::process::{Command as StdCommand, Stdio};

    let mut child = StdCommand::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    if let Some(stdin) = child.stdin.as_mut() {
        stdin.write_all(text.as_bytes())?;
    }
    child.wait()?;
    Ok(())
}
