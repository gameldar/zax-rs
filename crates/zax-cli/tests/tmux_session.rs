#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
//! Boots a real tmux server on a socket of its own and asserts the session it
//! builds. Skipped when tmux is absent, which is the case outside `nix develop`.

use std::path::Path;
use std::process::Command;

use tempfile::TempDir;
use zax_app::{Multiplexer, StartSession, StartSessionOutcome, StartSessionRequest};
use zax_domain::{
    BranchName, Command as ZaxCommand, LayoutSpec, PaneKind, PaneSize, PaneSpec, RootRepoName,
    SessionName,
};
use zax_exec::{RecordingHandover, SystemExec};
use zax_infra_git::GitCli;
use zax_infra_tmux::{conf, TmuxCli};

fn tmux_available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// A unique socket per test run, so concurrent runs cannot collide.
fn socket_name(label: &str) -> String {
    format!("zax-test-{}-{label}", std::process::id())
}

/// Kills the tmux server on this socket when dropped, on every exit path
/// including a panicking assertion. Without this a failed assertion in the
/// middle of a test would return early and leave the server running on its
/// temp socket for the rest of the machine's life.
struct TmuxServerGuard {
    socket: String,
}

impl TmuxServerGuard {
    fn new(socket: &str) -> Self {
        Self {
            socket: socket.to_owned(),
        }
    }
}

impl Drop for TmuxServerGuard {
    fn drop(&mut self) {
        // Best effort: if the server never started (e.g. we panicked before
        // creating a session), `kill-server` just fails quietly.
        let _ = Command::new("tmux")
            .args(["-L", &self.socket, "kill-server"])
            .output();
    }
}

/// `RecordingHandover`, never `SystemHandover`. `SystemHandover` calls
/// `execve`, which on success REPLACES this test process with tmux - every
/// assertion after the call would be unreachable and the test would pass by
/// never running. Recording the attach instead lets the test assert on the
/// session tmux actually built.
fn tmux_cli(socket: &str, conf_dir: &Path) -> TmuxCli<SystemExec, RecordingHandover> {
    let conf_path = conf::write_to(conf_dir, "C-Space").unwrap();
    TmuxCli::new(
        SystemExec,
        RecordingHandover::default(),
        socket.to_owned(),
        conf_path,
    )
}

/// Ask the real tmux how many panes the session has. This is independent of
/// zax's own bookkeeping: it queries the live server rather than anything
/// `TmuxCli` remembers about the calls it made.
fn pane_indexes(socket: &str, conf: &Path, session: &SessionName) -> Vec<String> {
    let output = Command::new("tmux")
        .args([
            "-L",
            socket,
            "-f",
            &conf.to_string_lossy(),
            "list-panes",
            "-t",
            &format!("{}:0", session.as_str()),
            "-F",
            "#{pane_index}",
        ])
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_owned)
        .collect()
}

/// A repo called `acme` on branch `OMEGA-1963-stamp`, with one commit.
fn git_fixture(root: &Path) -> std::path::PathBuf {
    let repo = root.join("src").join("acme");
    std::fs::create_dir_all(&repo).unwrap();
    let run = |args: &[&str]| {
        let status = Command::new("git")
            .args(args)
            .current_dir(&repo)
            .env("GIT_AUTHOR_NAME", "zax test")
            .env("GIT_AUTHOR_EMAIL", "zax@example.invalid")
            .env("GIT_COMMITTER_NAME", "zax test")
            .env("GIT_COMMITTER_EMAIL", "zax@example.invalid")
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    };
    run(&["init", "--initial-branch=main", "--quiet"]);
    std::fs::write(repo.join("README.md"), "zax\n").unwrap();
    run(&["add", "README.md"]);
    run(&["commit", "--quiet", "-m", "Add README"]);
    run(&["checkout", "--quiet", "-b", "OMEGA-1963-stamp"]);
    repo
}

/// The session name zax should derive, built the same way `SessionName::derive`
/// is documented to build it - from the repo and branch, not by string-pasting
/// a name and hoping it matches whatever the code under test produces.
fn expected_session_name() -> SessionName {
    SessionName::derive(
        &RootRepoName::parse("acme").unwrap(),
        &BranchName::parse_local("OMEGA-1963-stamp").unwrap(),
    )
}

#[test]
fn a_session_is_built_with_one_pane_per_layout_entry() {
    if !tmux_available() {
        eprintln!("skipping: tmux is not on PATH (run inside `nix develop`)");
        return;
    }

    let tmp = TempDir::new().unwrap();
    let socket = socket_name("layout");
    let _guard = TmuxServerGuard::new(&socket);
    let repo = git_fixture(tmp.path());
    let conf_dir = tmp.path().join("conf");
    let tmux = tmux_cli(&socket, &conf_dir);
    let conf_path = conf_dir.join("tmux.conf");
    let git = GitCli::new(SystemExec, repo.clone());

    // `sleep` rather than a real agent: the panes only have to exist.
    let sleep = |secs: &str| ZaxCommand::new(vec!["sleep".to_owned(), secs.to_owned()]).unwrap();
    let layout = LayoutSpec::new(
        sleep("30"),
        vec![
            PaneSpec {
                kind: PaneKind::Raw {
                    command: sleep("30"),
                },
                size: Some(PaneSize::percent(30).unwrap()),
            },
            PaneSpec {
                kind: PaneKind::Raw {
                    command: sleep("30"),
                },
                size: Some(PaneSize::percent(25).unwrap()),
            },
        ],
    )
    .unwrap();

    let name = expected_session_name();

    let outcome = StartSession::new(&tmux, &git)
        .run(StartSessionRequest {
            cwd: repo.clone(),
            zax_bin: std::path::PathBuf::from("/bin/true"),
            layout,
            inside_multiplexer: false,
        })
        .unwrap();

    assert!(matches!(outcome, StartSessionOutcome::Created(_)));
    assert_eq!(
        tmux.handover_ref().calls().len(),
        1,
        "the session should have been attached to exactly once"
    );
    assert!(
        tmux.session_exists(&name).unwrap(),
        "the session should exist"
    );
    assert_eq!(
        pane_indexes(&socket, &conf_path, &name),
        ["0", "1", "2"],
        "one agent pane plus one pane per layout entry"
    );

    tmux.kill_session(&name).unwrap();
    assert!(!tmux.session_exists(&name).unwrap());
}

#[test]
fn a_second_start_attaches_instead_of_rebuilding() {
    if !tmux_available() {
        eprintln!("skipping: tmux is not on PATH (run inside `nix develop`)");
        return;
    }

    let tmp = TempDir::new().unwrap();
    let socket = socket_name("attach");
    let _guard = TmuxServerGuard::new(&socket);
    let repo = git_fixture(tmp.path());
    let conf_dir = tmp.path().join("conf");
    let tmux = tmux_cli(&socket, &conf_dir);
    let conf_path = conf_dir.join("tmux.conf");
    let git = GitCli::new(SystemExec, repo.clone());

    let sleep = ZaxCommand::new(vec!["sleep".to_owned(), "30".to_owned()]).unwrap();
    let layout = LayoutSpec::new(
        sleep.clone(),
        vec![PaneSpec {
            kind: PaneKind::Raw { command: sleep },
            size: Some(PaneSize::percent(30).unwrap()),
        }],
    )
    .unwrap();
    let request = || StartSessionRequest {
        cwd: repo.clone(),
        zax_bin: std::path::PathBuf::from("/bin/true"),
        layout: layout.clone(),
        inside_multiplexer: false,
    };

    let name = expected_session_name();

    let first = StartSession::new(&tmux, &git).run(request()).unwrap();
    assert!(matches!(first, StartSessionOutcome::Created(_)));
    assert_eq!(pane_indexes(&socket, &conf_path, &name), ["0", "1"]);

    let second = StartSession::new(&tmux, &git).run(request()).unwrap();
    assert!(
        matches!(second, StartSessionOutcome::Attached(_)),
        "a second start must attach, not rebuild"
    );
    assert_eq!(
        pane_indexes(&socket, &conf_path, &name),
        ["0", "1"],
        "a second start must not add panes"
    );
}

#[test]
fn the_server_runs_on_our_socket_and_not_the_default_one() {
    if !tmux_available() {
        eprintln!("skipping: tmux is not on PATH (run inside `nix develop`)");
        return;
    }

    let tmp = TempDir::new().unwrap();
    let socket = socket_name("socket");
    let _guard = TmuxServerGuard::new(&socket);
    let repo = git_fixture(tmp.path());
    let tmux = tmux_cli(&socket, &tmp.path().join("conf"));
    let git = GitCli::new(SystemExec, repo.clone());

    let sleep = ZaxCommand::new(vec!["sleep".to_owned(), "30".to_owned()]).unwrap();
    let layout = LayoutSpec::new(sleep, vec![]).unwrap();
    StartSession::new(&tmux, &git)
        .run(StartSessionRequest {
            cwd: repo,
            zax_bin: std::path::PathBuf::from("/bin/true"),
            layout,
            inside_multiplexer: false,
        })
        .unwrap();

    // Our socket has the session.
    assert!(!tmux.list_sessions().unwrap().is_empty());

    // The default socket must be untouched by anything zax did.
    let default = Command::new("tmux")
        .args(["list-sessions"])
        .output()
        .unwrap();
    let listed = String::from_utf8_lossy(&default.stdout);
    assert!(
        !listed.contains("zax-acme"),
        "zax must not create sessions on the default socket: {listed}"
    );
}

#[test]
fn a_start_while_the_session_is_attached_builds_a_second_one() {
    if !tmux_available() {
        eprintln!("skipping: tmux is not on PATH (run inside `nix develop`)");
        return;
    }

    let tmp = TempDir::new().unwrap();
    let socket = socket_name("sibling");
    let _guard = TmuxServerGuard::new(&socket);
    let repo = git_fixture(tmp.path());
    let tmux = tmux_cli(&socket, &tmp.path().join("conf"));
    let git = GitCli::new(SystemExec, repo.clone());

    let sleep = ZaxCommand::new(vec!["sleep".to_owned(), "30".to_owned()]).unwrap();
    let request = || StartSessionRequest {
        cwd: repo.clone(),
        zax_bin: std::path::PathBuf::from("/bin/true"),
        layout: LayoutSpec::new(sleep.clone(), vec![]).unwrap(),
        inside_multiplexer: false,
    };

    let first = expected_session_name();
    StartSession::new(&tmux, &git).run(request()).unwrap();

    // A control-mode client is a real attached client without needing a pty.
    // It stays attached for as long as its stdin is open.
    let mut client = Command::new("tmux")
        .args(["-L", &socket, "-C", "attach-session", "-t"])
        .arg(format!("={}", first.as_str()))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !tmux
        .list_sessions()
        .unwrap()
        .iter()
        .any(|s| s.name == first && s.attached)
    {
        assert!(
            std::time::Instant::now() < deadline,
            "the control client never attached"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    let second = StartSession::new(&tmux, &git).run(request()).unwrap();
    let _ = client.kill();
    let _ = client.wait();

    let expected = SessionName::derive_slot(
        &RootRepoName::parse("acme").unwrap(),
        &BranchName::parse_local("OMEGA-1963-stamp").unwrap(),
        zax_domain::SessionSlot::new(2).unwrap(),
    );
    assert_eq!(second, StartSessionOutcome::Created(expected.clone()));
    assert!(
        tmux.session_exists(&first).unwrap(),
        "the first agent survives"
    );
    assert!(tmux.session_exists(&expected).unwrap());
}
