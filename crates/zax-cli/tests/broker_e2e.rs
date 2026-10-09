#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
//! The real `zax broker`, spawned the way a pane spawns it, serving the work
//! topic to two clients.

use std::time::Duration;

use zax_broker::client::{Client, Event};
use zax_broker::topics::{WorkSpec, WORK};
use zax_broker::BrokerPaths;
use zax_protocol::SourceKey;

#[test]
fn a_pane_spawns_the_broker_and_a_second_pane_shares_it() {
    let dir = tempfile::tempdir().unwrap();
    let paths = BrokerPaths::in_dir(dir.path(), dir.path());
    let zax = env!("CARGO_BIN_EXE_zax");
    let spawn = vec![
        "env".to_owned(),
        format!("ZAX_BROKER_DIR={}", dir.path().display()),
        zax.to_owned(),
        "broker".to_owned(),
    ];
    let key = SourceKey::new(
        WORK,
        &WorkSpec {
            worktree_root: dir.path().join("wt").display().to_string(),
        },
    );

    let a = Client::connect_or_spawn(&paths, 1, &spawn).unwrap();
    let b = Client::connect_or_spawn(&paths, 1, &spawn).unwrap();
    a.subscribe(key.clone()).unwrap();
    b.subscribe(key).unwrap();
    for client in [&a, &b] {
        match client
            .events()
            .recv_timeout(Duration::from_secs(20))
            .unwrap()
        {
            // gh may be absent or unauthenticated in CI: either a value or an
            // error is a snapshot, and a snapshot is what proves the broker.
            Event::Snapshot(_) => {}
            Event::Disconnected(why) => panic!("{why}"),
        }
    }
}

#[test]
fn an_add_from_one_pane_reaches_another() {
    use zax_broker::topics::{decode_todo, encode_todo_edit, TodoSpec, TODO};
    use zax_domain::TodoEdit;

    let dir = tempfile::tempdir().unwrap();
    let paths = BrokerPaths::in_dir(dir.path(), dir.path());
    let zax = env!("CARGO_BIN_EXE_zax");
    let spawn = vec![
        "env".to_owned(),
        format!("ZAX_BROKER_DIR={}", dir.path().display()),
        zax.to_owned(),
        "broker".to_owned(),
    ];
    let file = dir.path().join("todo.md");
    let key = SourceKey::new(
        TODO,
        &TodoSpec {
            file: file.display().to_string(),
        },
    );

    let a = Client::connect_or_spawn(&paths, 1, &spawn).unwrap();
    let b = Client::connect_or_spawn(&paths, 1, &spawn).unwrap();
    a.subscribe(key.clone()).unwrap();
    b.subscribe(key.clone()).unwrap();

    let (method, params) = encode_todo_edit(&TodoEdit::Add {
        text: "shared".into(),
        due: None,
    });
    a.call(key, method, params, Duration::from_secs(20))
        .unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        match b.events().recv_timeout(left).unwrap() {
            Event::Snapshot(s) => {
                let items = s
                    .value
                    .as_ref()
                    .map(|v| decode_todo(v).unwrap().items)
                    .unwrap_or_default();
                if items.iter().any(|i| i.text == "shared") {
                    break;
                }
            }
            Event::Disconnected(why) => panic!("{why}"),
        }
    }
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "- [ ] shared\n");
}
