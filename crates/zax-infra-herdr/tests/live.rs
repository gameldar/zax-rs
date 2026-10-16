//! Against a running herdr server. Ignored by default: each test opens a
//! real herdr space. Run with `cargo test -p zax-infra-herdr -- --ignored`
//! while herdr is running.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::path::Path;
use std::process::Command as Process;

use zax_app::{Editor, WorktreeOpener};
use zax_domain::Command;
use zax_exec::SystemExec;
use zax_infra_herdr::{HerdrCli, HerdrEditor};

fn herdr() -> HerdrCli<SystemExec> {
    HerdrCli::new(SystemExec, "herdr".to_owned())
}

fn spaces() -> String {
    let out = Process::new("herdr")
        .args(["workspace", "list"])
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap()
}

#[test]
#[ignore = "opens a real herdr space"]
fn the_server_answers_a_probe() {
    let detail = WorktreeOpener::probe(&herdr()).unwrap();
    assert!(detail.contains("server running"), "{detail}");
}

#[test]
#[ignore = "opens a real herdr space"]
fn an_edit_whose_editor_exits_leaves_no_space_behind() {
    // `true` stands in for an editor the user quits at once.
    let dir = tempfile::tempdir().unwrap();
    let name = format!("zax-live-{}.md", std::process::id());
    let file = dir.path().join(&name);
    std::fs::write(&file, "- [ ] live\n").unwrap();
    let editor = HerdrEditor::new(herdr(), Command::new(vec!["true".to_owned()]).unwrap());

    editor.edit(&file).unwrap();

    for _ in 0..20 {
        if !spaces().contains(&name) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    panic!("the space labelled {name} is still open:\n{}", spaces());
}

#[test]
#[ignore = "opens a real herdr space"]
fn opening_a_worktree_twice_focuses_the_space_the_first_opened() {
    let path = std::env::var("ZAX_LIVE_WORKTREE").expect("set ZAX_LIVE_WORKTREE");
    let path = Path::new(&path);
    let opener = herdr();

    opener.open_worktree(path).unwrap();
    let after_first = spaces();
    opener.open_worktree(path).unwrap();

    assert_eq!(
        after_first.matches("workspace_id").count(),
        spaces().matches("workspace_id").count(),
        "the second open made another space"
    );
}
