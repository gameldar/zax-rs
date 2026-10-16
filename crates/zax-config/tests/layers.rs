#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::fs;
use std::path::Path;

use tempfile::TempDir;
use zax_domain::{PaneKind, RootRepoName};

fn write(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, text).unwrap();
}

#[test]
fn all_three_layers_are_read_in_order() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config");
    let repo_dir = tmp.path().join("src").join("acme");
    let repo = RootRepoName::parse("acme").unwrap();

    write(
        &zax_config::global_config_path(&config_home),
        "[agent]\ncommand = [\"claude\"]\n[tmux]\nprefix = \"C-Space\"\n",
    );
    write(
        &zax_config::repo_config_path(&repo_dir),
        "[worktree]\nhooks = [\"direnv allow\"]\n",
    );
    write(
        &zax_config::override_config_path(&config_home, "acme"),
        "[tmux]\nprefix = \"C-q\"\n",
    );

    let config = zax_config::load(&config_home, &repo_dir, &repo, "/home/me").unwrap();

    assert_eq!(config.tmux.prefix, "C-q", "your layer wins");
    assert_eq!(config.layout.agent().unwrap().argv(), ["claude"]);
    assert_eq!(config.worktree.hooks.len(), 1);
}

#[test]
fn a_missing_layer_is_an_empty_layer_not_an_error() {
    let tmp = TempDir::new().unwrap();
    let repo = RootRepoName::parse("acme").unwrap();
    let config = zax_config::load(
        &tmp.path().join("config"),
        &tmp.path().join("src").join("acme"),
        &repo,
        "/home/me",
    )
    .unwrap();
    assert_eq!(config.tmux.prefix, "C-Space");
}

#[test]
fn a_name_key_redirects_which_override_directory_is_read() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config");
    let repo_dir = tmp.path().join("src").join("acme");
    let repo = RootRepoName::parse("acme").unwrap();

    write(
        &zax_config::repo_config_path(&repo_dir),
        "name = \"acme-backend\"\n",
    );
    write(
        &zax_config::override_config_path(&config_home, "acme"),
        "[tmux]\nprefix = \"WRONG\"\n",
    );
    write(
        &zax_config::override_config_path(&config_home, "acme-backend"),
        "[tmux]\nprefix = \"C-q\"\n",
    );

    let config = zax_config::load(&config_home, &repo_dir, &repo, "/home/me").unwrap();
    assert_eq!(config.tmux.prefix, "C-q");
}

#[test]
fn a_name_key_that_is_not_a_single_directory_name_is_rejected() {
    // The layer that is supposed to lose picks the identity of the layer that
    // beats it, so an unchecked `name` lets a checked-in `.zax.toml` read a
    // file of its own choosing as your personal override, or point the lookup
    // at a directory that will never exist and so suppress yours entirely.
    for name in [
        "../../../src/tools/their-repo/zax-override",
        "..",
        ".",
        "/etc/zax",
        "acme/omega",
        "",
    ] {
        let tmp = TempDir::new().unwrap();
        let config_home = tmp.path().join("config");
        let repo_dir = tmp.path().join("src").join("acme");
        let repo = RootRepoName::parse("acme").unwrap();
        write(
            &zax_config::repo_config_path(&repo_dir),
            &format!("name = {}\n", toml_string(name)),
        );

        let err = zax_config::load(&config_home, &repo_dir, &repo, "/home/me")
            .expect_err("`{name}` must be rejected");
        let message = err.to_string();
        assert!(message.contains("name"), "got: {message}");
        assert!(
            message.contains(".zax.toml"),
            "the error must name the file it came from: {message}"
        );
    }
}

/// A TOML string literal, escaped independently of anything under test.
fn toml_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

#[test]
fn tuning_a_repo_you_do_not_own_never_touches_its_working_tree() {
    // The override lives entirely outside the repo directory.
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config");
    let repo_dir = tmp.path().join("src").join("acme");
    fs::create_dir_all(&repo_dir).unwrap();
    let repo = RootRepoName::parse("acme").unwrap();

    write(
        &zax_config::override_config_path(&config_home, "acme"),
        "[[layout.panes]]\nkind = \"raw\"\ncommand = [\"lazygit\"]\n",
    );

    let config = zax_config::load(&config_home, &repo_dir, &repo, "/home/me").unwrap();

    assert!(matches!(
        config.layout.panes()[0].kind,
        PaneKind::Raw { .. }
    ));
    assert_eq!(
        fs::read_dir(&repo_dir).unwrap().count(),
        0,
        "the repo directory must be untouched"
    );
}

#[test]
fn a_repo_layout_is_replaced_wholesale_by_a_personal_one() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config");
    let repo_dir = tmp.path().join("src").join("acme");
    let repo = RootRepoName::parse("acme").unwrap();

    write(
        &zax_config::repo_config_path(&repo_dir),
        "[[layout.panes]]\nkind = \"builtin\"\nname = \"prs-for-review\"\n\n\
         [[layout.panes]]\nkind = \"builtin\"\nname = \"branch-status\"\n",
    );
    write(
        &zax_config::override_config_path(&config_home, "acme"),
        "[[layout.panes]]\nkind = \"raw\"\ncommand = [\"lazygit\"]\n",
    );

    let config = zax_config::load(&config_home, &repo_dir, &repo, "/home/me").unwrap();
    assert_eq!(config.layout.panes().len(), 1);
}

#[test]
fn a_broken_toml_file_names_the_file_it_came_from() {
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config");
    let repo_dir = tmp.path().join("src").join("acme");
    let repo = RootRepoName::parse("acme").unwrap();

    write(&zax_config::global_config_path(&config_home), "[agent\n");

    let err = zax_config::load(&config_home, &repo_dir, &repo, "/home/me").unwrap_err();
    assert!(err.to_string().contains("config.toml"), "got: {err}");
}

#[test]
fn your_layer_can_narrow_the_review_queue_a_repo_did_not_ask_about() {
    // The `[prs]` filter is about your review queue across every repository,
    // so the layer that should usually win it is your own.
    let tmp = TempDir::new().unwrap();
    let config_home = tmp.path().join("config");
    let repo_dir = tmp.path().join("src").join("acme");
    let repo = RootRepoName::parse("acme").unwrap();

    write(
        &zax_config::global_config_path(&config_home),
        "[prs]\ninclude = [\"acme/*\"]\nmax_age = \"30d\"\n",
    );
    write(
        &zax_config::override_config_path(&config_home, "acme"),
        "[prs]\nexclude = [\"acme/legacy-api\"]\n",
    );

    let config = zax_config::load(&config_home, &repo_dir, &repo, "/home/me").unwrap();

    assert_eq!(
        config
            .prs
            .include
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        ["acme/*"]
    );
    assert_eq!(
        config
            .prs
            .exclude
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        ["acme/legacy-api"]
    );
    assert_eq!(
        config.prs.max_age,
        Some(std::time::Duration::from_secs(30 * 86_400))
    );
}
