//! The pane processes: a terminal loop over a `zax-tui` pane.
//!
//! Everything this loop decides is already tested in `zax-tui`. What is left
//! here is wiring: raw mode, a fetch thread, and turning a `PaneAction` into a
//! side effect.

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use zax_app::queries;
use zax_domain::keymap::PaneContext;
use zax_domain::{parse_duration, Command, ReviewPass, ReviewScope};
use zax_exec::{Exec, SystemExec};
use zax_tui::{
    refresh, BranchStatusPane, ExecPane, MyPrsPane, PaneAction, PrsForReviewPane, ReviewMode,
};

use crate::cli::PaneCommand;
use crate::marked;
use crate::screen::Screen;
use crate::wiring;

const JITTER: f64 = 0.15;
/// How long the loop waits for a keystroke before re-checking the clock.
const TICK: Duration = Duration::from_millis(250);

pub fn run(command: PaneCommand) -> Result<()> {
    match command {
        PaneCommand::PrsForReview => run_prs_pane(),
        PaneCommand::BranchStatus => run_branch_pane(),
        PaneCommand::MyPrs => run_my_prs_pane(),
        PaneCommand::Tickets => crate::cmd_tickets::run(),
        PaneCommand::Todo => crate::cmd_todo::run(),
        PaneCommand::Worktrees { ticket } => crate::cmd_worktrees::run(&ticket),
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
                        other => {
                            // A side effect that failed is reported through the
                            // pane's own error channel, which marks it stale
                            // and shows the line. The pane owns the screen, so
                            // there is nowhere else for it to go.
                            if let Some(message) = $on_action(other)? {
                                pane.apply(Err(message), SystemTime::now());
                            }
                        }
                    },
                    // Refreshing on focus is what the spec asks for. The flag
                    // has to be cleared again, or every pane draws as the
                    // focused one for the rest of the session: all headers
                    // bold at once, and a reversed "selected" row in
                    // prs-for-review while the cursor is in the agent pane.
                    Event::FocusGained => {
                        pane.set_focused(true);
                        due = Instant::now();
                    }
                    Event::FocusLost => pane.set_focused(false),
                    Event::Mouse(mouse) => pane.on_mouse(mouse),
                    _ => {}
                }
            }
        }
        Ok(())
    }};
}

fn run_prs_pane() -> Result<()> {
    let cwd = std::env::current_dir()?;
    // The pane reads the same three-layer config the session was started from,
    // for the `[prs]` filter. A config this pane cannot read is worth failing
    // over: the alternative is a pane quietly showing the unfiltered queue.
    let config = wiring::resolve()?.config;
    let filter = config.prs;
    let (request, results) = spawn_fetcher(move || {
        let gh = zax_infra_github::GhCli::new(SystemExec, cwd.clone());
        queries::list_review_queue(&gh, &filter, SystemTime::now()).map_err(|e| e.to_string())
    });
    pane_loop!(
        PrsForReviewPane::new(
            config
                .keymaps
                .for_context(PaneContext::PrsForReview)
                .clone()
        ),
        Duration::from_secs(120),
        request,
        results,
        perform
    )
}

fn run_my_prs_pane() -> Result<()> {
    let cwd = std::env::current_dir()?;
    // The same `[prs]` filter the review queue reads, minus its max_age, which
    // `list_my_pull_requests` does not apply. A config this pane cannot read is
    // worth failing over: the alternative is quietly showing an unfiltered one.
    let config = wiring::resolve()?.config;
    let filter = config.prs;
    let (request, results) = spawn_fetcher(move || {
        let gh = zax_infra_github::GhCli::new(SystemExec, cwd.clone());
        queries::list_my_pull_requests(&gh, &filter).map_err(|e| e.to_string())
    });
    pane_loop!(
        MyPrsPane::new(config.keymaps.for_context(PaneContext::MyPrs).clone()),
        Duration::from_secs(120),
        request,
        results,
        perform
    )
}

fn run_branch_pane() -> Result<()> {
    let cwd = std::env::current_dir()?;
    let config = wiring::resolve()?.config;
    let (request, results) = spawn_fetcher(move || {
        let git = zax_infra_git::GitCli::new(SystemExec, cwd.clone());
        let gh = zax_infra_github::GhCli::new(SystemExec, cwd.clone());
        queries::branch_view(&git, &gh).map_err(|e| e.to_string())
    });
    pane_loop!(
        BranchStatusPane::new(
            config
                .keymaps
                .for_context(PaneContext::BranchStatus)
                .clone()
        ),
        Duration::from_secs(60),
        request,
        results,
        perform
    )
}

fn run_exec_pane(title: String, every: Duration, command: Command) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let config = wiring::resolve()?.config;
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
        ExecPane::new(
            title,
            every,
            config.keymaps.for_context(PaneContext::Exec).clone()
        ),
        every,
        request,
        results,
        perform
    )
}

/// Turn a row action into a side effect, and return the one line the pane
/// should show if it went wrong. Never fatal: a pane must survive a missing
/// `open` or clipboard tool.
///
/// Opening a URL and copying to the clipboard stay silent by design - there is
/// nothing to report but a missing helper program, and the user can see that
/// nothing opened. Building a review worktree is different: it mutates the
/// repository, it can refuse for reasons the user needs to read, and a pane
/// that swallowed the answer would leave a worktree key press looking like a no-op.
pub(crate) fn perform(action: PaneAction) -> Result<Option<String>> {
    perform_with(&SystemExec, action)
}

fn perform_with<E: Exec>(exec: &E, action: PaneAction) -> Result<Option<String>> {
    match action {
        PaneAction::OpenUrl(url) => {
            let _ = exec.exec(open_program(), &[url], None, &[]);
        }
        PaneAction::CopyUrl(url) => copy_to_clipboard(&url),
        PaneAction::ReviewWorktree { repo, number, mode } => {
            let zax = wiring::zax_bin()?.to_string_lossy().into_owned();
            let mut args = vec![
                "worktree".to_owned(),
                "review".to_owned(),
                format!("https://github.com/{repo}/pull/{number}"),
            ];
            match mode {
                ReviewMode::Open => args.push("--no-review".to_owned()),
                ReviewMode::OpenAndReview(run) => {
                    if run.pass == ReviewPass::Again {
                        args.push("--re-review".to_owned());
                    }
                    if run.scope == ReviewScope::Stack {
                        args.push("--stack".to_owned());
                    }
                }
            }
            let result = exec.exec(&zax, &args, None, &[]);
            return Ok(review_message(result));
        }
        // The tickets loop performs its own six row actions, and the todo
        // loop its two; they need the broker connection, which this function
        // does not have.
        PaneAction::None
        | PaneAction::Refresh
        | PaneAction::Quit
        | PaneAction::StartWork { .. }
        | PaneAction::ChooseRepo { .. }
        | PaneAction::OpenWorktree { .. }
        | PaneAction::ListTransitions { .. }
        | PaneAction::Transition { .. }
        | PaneAction::ListWorktrees { .. }
        | PaneAction::Todo(_)
        | PaneAction::OpenFile(_) => {}
    }
    Ok(None)
}

/// What a finished `zax worktree review` should tell the pane.
///
/// Two outcomes have something to say, and neither of them is silence. A
/// refused review exits non-zero, and zax prints its whole error chain as the
/// last line of stderr, so that line is the message; anything else (a tool
/// that wrote nothing, a status with no explanation) still produces a line,
/// because the point is that a failed worktree action is never silent. A worktree that was
/// made but could not be opened exits zero and warns, and that warning is just
/// as invisible to the user as a refusal would be: the editor never appeared
/// either way.
fn review_message(result: Result<zax_exec::Output, zax_exec::ExecError>) -> Option<String> {
    let detail = match result {
        Ok(out) if out.ok() => marked::find_warning(&out.stderr)?,
        // zax's own line, which names the reason. Only when it never got to
        // write one does the tool's own last word stand in, because a failed
        // a worktree action falling silent is the thing being prevented.
        Ok(out) => marked::find_error(&out.stderr)
            .or_else(|| {
                out.stderr
                    .lines()
                    .rev()
                    .map(str::trim)
                    .find(|line| !line.is_empty())
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| format!("exited with status {}", out.status)),
        Err(error) => error.to_string(),
    };
    Some(format!("worktree review: {detail}"))
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use zax_domain::{ReviewPass, ReviewRun};
    use zax_exec::{ExecError, Output, RecordingExec};
    use zax_tui::{PaneAction, ReviewMode};

    use super::{perform_with, review_message};

    fn out(status: i32, stderr: &str) -> Result<Output, ExecError> {
        Ok(Output {
            status,
            stdout: String::new(),
            stderr: stderr.to_owned(),
        })
    }

    #[test]
    fn a_review_that_worked_says_nothing() {
        assert_eq!(review_message(out(0, "")), None);
    }

    #[test]
    fn a_refused_review_reaches_the_pane_instead_of_vanishing() {
        // The two refusals a worktree key press can actually meet, spelled the way zax
        // prints them.
        let refusal = "zax: pull request #42 belongs to acme/omega, but this \
                       is someone/zed-ai-extensions";
        let message = review_message(out(1, refusal)).unwrap();
        assert!(message.contains("acme/omega"), "got: {message}");
        assert!(
            message.contains("someone/zed-ai-extensions"),
            "got: {message}"
        );
    }

    #[test]
    fn only_the_final_line_of_the_error_chain_is_shown() {
        // A pane row is one line, and zax puts its whole chain on the last one.
        let message = review_message(out(1, "warning: noise\nzax: the real problem\n")).unwrap();
        assert!(message.ends_with("the real problem"), "got: {message}");
        assert!(!message.contains("noise"), "got: {message}");
    }

    #[test]
    fn a_failure_with_nothing_to_say_still_says_something() {
        let message = review_message(out(2, "   \n")).unwrap();
        assert!(message.contains('2'), "got: {message}");
    }

    #[test]
    fn a_failed_review_row_action_hands_the_pane_a_line_to_show() {
        // The worktree key press mutates the repository and can refuse; discarding its
        // result leaves the keystroke looking like a no-op.
        let exec = RecordingExec::with_responses(vec![Output {
            status: 1,
            stdout: String::new(),
            stderr: "zax: branch `patch-1` is at aaaaaaa\n".to_owned(),
        }]);
        let action = PaneAction::ReviewWorktree {
            repo: zax_domain::RepoSlug::parse("acme/omega").unwrap(),
            number: 42,
            mode: ReviewMode::OpenAndReview(ReviewRun::single(ReviewPass::First)),
        };

        let message = perform_with(&exec, action).unwrap().unwrap();

        assert!(message.contains("patch-1"), "got: {message}");
        assert_eq!(
            exec.args(0),
            [
                "worktree",
                "review",
                "https://github.com/acme/omega/pull/42"
            ]
        );
    }

    #[test]
    fn a_review_row_action_that_worked_leaves_the_pane_alone() {
        let exec = RecordingExec::returning("");
        let action = PaneAction::ReviewWorktree {
            repo: zax_domain::RepoSlug::parse("acme/omega").unwrap(),
            number: 42,
            mode: ReviewMode::OpenAndReview(ReviewRun::single(ReviewPass::First)),
        };
        assert_eq!(perform_with(&exec, action).unwrap(), None);
    }

    #[test]
    fn a_follow_up_review_passes_re_review_to_the_worktree_command() {
        let exec = RecordingExec::returning("");
        let action = PaneAction::ReviewWorktree {
            repo: zax_domain::RepoSlug::parse("acme/omega").unwrap(),
            number: 42,
            mode: ReviewMode::OpenAndReview(ReviewRun::single(ReviewPass::Again)),
        };

        assert_eq!(perform_with(&exec, action).unwrap(), None);

        assert_eq!(
            exec.args(0),
            [
                "worktree",
                "review",
                "https://github.com/acme/omega/pull/42",
                "--re-review"
            ]
        );
    }

    #[test]
    fn a_stack_review_passes_stack_to_the_worktree_command() {
        let exec = RecordingExec::returning("");
        let action = PaneAction::ReviewWorktree {
            repo: zax_domain::RepoSlug::parse("acme/omega").unwrap(),
            number: 42,
            mode: ReviewMode::OpenAndReview(ReviewRun::stack(ReviewPass::Again)),
        };

        assert_eq!(perform_with(&exec, action).unwrap(), None);

        assert_eq!(
            exec.args(0),
            [
                "worktree",
                "review",
                "https://github.com/acme/omega/pull/42",
                "--re-review",
                "--stack"
            ]
        );
    }

    #[test]
    fn opening_without_a_review_passes_no_review_to_the_worktree_command() {
        let exec = RecordingExec::returning("");
        let action = PaneAction::ReviewWorktree {
            repo: zax_domain::RepoSlug::parse("acme/omega").unwrap(),
            number: 42,
            mode: ReviewMode::Open,
        };

        assert_eq!(perform_with(&exec, action).unwrap(), None);

        assert_eq!(
            exec.args(0),
            [
                "worktree",
                "review",
                "https://github.com/acme/omega/pull/42",
                "--no-review"
            ]
        );
    }

    #[test]
    fn a_worktree_that_was_made_but_not_opened_still_says_so() {
        // The editor failing is a warning and an exit of zero, because the
        // worktree is there. The keystroke still looks like a no-op unless the
        // part that did not happen reaches the pane.
        let stderr = "zax: warning: could not open the worktree in the editor: \
                      zed is not installed or not on PATH\n  if Zed is installed, \
                      open it and run `cli: install`\n";

        let message = review_message(out(0, stderr)).unwrap();

        assert!(message.contains("could not open"), "got: {message}");
        assert!(message.contains("zed"), "got: {message}");
    }

    #[test]
    fn a_hook_writing_to_stderr_is_not_mistaken_for_a_warning() {
        // Setup hooks inherit zax's stderr, so a chatty one lands in the same
        // place; only zax's own marked line is something to show.
        assert_eq!(review_message(out(0, "npm warn deprecated glob@7\n")), None);
    }

    #[test]
    fn a_refused_review_shows_the_cause_and_not_gits_closing_advice() {
        // git's remote failure ends with boilerplate, every time, so the last
        // line of stderr is the one line guaranteed not to be the reason.
        let stderr = "zax: git exited with status 128: git@github.com: Permission denied \
                      (publickey).\nfatal: Could not read from remote repository.\n\n\
                      Please make sure you have the correct access rights\nand the \
                      repository exists.\n";

        let message = review_message(out(128, stderr)).unwrap();

        assert!(message.contains("Permission denied"), "got: {message}");
        assert!(
            !message.contains("repository exists"),
            "the pane showed git's closing advice: {message}"
        );
    }

    #[test]
    fn a_tool_that_failed_without_zax_having_a_say_still_reports_something() {
        // Nothing marked means zax never got to describe it - a pane must
        // still show a line rather than fall silent.
        let message = review_message(out(1, "some tool fell over\n")).unwrap();

        assert!(message.contains("some tool fell over"), "got: {message}");
    }

    #[test]
    fn a_review_that_could_not_be_started_is_reported_too() {
        let message = review_message(Err(ExecError::NotFound {
            program: "zax".to_owned(),
        }))
        .unwrap();
        assert!(message.contains("zax"), "got: {message}");
    }
}
