//! Layer merging. Your layer wins; arrays replace.

use crate::raw::{
    RawAgent, RawConfig, RawEditor, RawLayout, RawOpen, RawProjects, RawPrs, RawTickets, RawTmux,
    RawTodo, RawWorktree,
};

/// Fold the layers in order, lowest precedence first.
pub fn merge(layers: Vec<RawConfig>) -> RawConfig {
    layers.into_iter().fold(RawConfig::default(), fold_one)
}

fn fold_one(mut base: RawConfig, over: RawConfig) -> RawConfig {
    base.name = over.name.or(base.name);
    base.agent = merge_agent(base.agent, over.agent);
    base.layout = merge_layout(base.layout, over.layout);
    base.worktree = merge_worktree(base.worktree, over.worktree);
    base.tmux = merge_tmux(base.tmux, over.tmux);
    base.prs = merge_prs(base.prs, over.prs);
    base.open = merge_open(base.open, over.open);
    base.editor = merge_editor(base.editor, over.editor);
    base.tickets = merge_tickets(base.tickets, over.tickets);
    base.todo = merge_todo(base.todo, over.todo);
    base.projects = merge_projects(base.projects, over.projects);
    base
}

fn merge_editor(base: Option<RawEditor>, over: Option<RawEditor>) -> Option<RawEditor> {
    match (base, over) {
        (b, None) => b,
        (None, o) => o,
        (Some(mut b), Some(o)) => {
            b.via = o.via.or(b.via);
            b.command = o.command.or(b.command);
            Some(b)
        }
    }
}

fn merge_open(base: Option<RawOpen>, over: Option<RawOpen>) -> Option<RawOpen> {
    match (base, over) {
        (b, None) => b,
        (None, o) => o,
        (Some(mut b), Some(o)) => {
            b.via = o.via.or(b.via);
            b.command = o.command.or(b.command);
            Some(b)
        }
    }
}

fn merge_tickets(base: Option<RawTickets>, over: Option<RawTickets>) -> Option<RawTickets> {
    match (base, over) {
        (b, None) => b,
        (None, o) => o,
        (Some(mut b), Some(o)) => {
            b.provider = o.provider.or(b.provider);
            Some(b)
        }
    }
}

fn merge_todo(base: Option<RawTodo>, over: Option<RawTodo>) -> Option<RawTodo> {
    match (base, over) {
        (b, None) => b,
        (None, o) => o,
        (Some(mut b), Some(o)) => {
            b.file = o.file.or(b.file);
            Some(b)
        }
    }
}

fn merge_agent(base: Option<RawAgent>, over: Option<RawAgent>) -> Option<RawAgent> {
    match (base, over) {
        (b, None) => b,
        (None, o) => o,
        (Some(mut b), Some(o)) => {
            b.command = o.command.or(b.command);
            Some(b)
        }
    }
}

fn merge_layout(base: Option<RawLayout>, over: Option<RawLayout>) -> Option<RawLayout> {
    match (base, over) {
        (b, None) => b,
        (None, o) => o,
        (Some(mut b), Some(o)) => {
            // Replacement, not extension.
            b.panes = o.panes.or(b.panes);
            Some(b)
        }
    }
}

fn merge_worktree(base: Option<RawWorktree>, over: Option<RawWorktree>) -> Option<RawWorktree> {
    match (base, over) {
        (b, None) => b,
        (None, o) => o,
        (Some(mut b), Some(o)) => {
            b.root = o.root.or(b.root);
            b.copy = o.copy.or(b.copy);
            b.hooks = o.hooks.or(b.hooks);
            Some(b)
        }
    }
}

fn merge_tmux(base: Option<RawTmux>, over: Option<RawTmux>) -> Option<RawTmux> {
    match (base, over) {
        (b, None) => b,
        (None, o) => o,
        (Some(mut b), Some(o)) => {
            b.prefix = o.prefix.or(b.prefix);
            Some(b)
        }
    }
}

fn merge_prs(base: Option<RawPrs>, over: Option<RawPrs>) -> Option<RawPrs> {
    match (base, over) {
        (b, None) => b,
        (None, o) => o,
        (Some(mut b), Some(o)) => {
            b.include = o.include.or(b.include);
            b.exclude = o.exclude.or(b.exclude);
            b.max_age = o.max_age.or(b.max_age);
            Some(b)
        }
    }
}

/// `projects` is honoured from the global layer only, so there is nothing for
/// a later layer to actually win here in practice - but the fold still has to
/// exist, since a later empty layer must not erase what the global layer set.
fn merge_projects(base: Option<RawProjects>, over: Option<RawProjects>) -> Option<RawProjects> {
    match (base, over) {
        (b, None) => b,
        (None, o) => o,
        (Some(mut b), Some(o)) => {
            b.root = o.root.or(b.root);
            b.paths = o.paths.or(b.paths);
            Some(b)
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use crate::raw::RawConfig;

    use super::*;

    fn parse(text: &str) -> RawConfig {
        toml::from_str(text).unwrap()
    }

    #[test]
    fn a_later_layer_wins_on_a_scalar() {
        let merged = merge(vec![
            parse("[agent]\ncommand = [\"claude\"]"),
            parse("[agent]\ncommand = [\"claude\", \"--dangerously-skip-permissions\"]"),
        ]);
        assert_eq!(
            merged.agent.unwrap().command.unwrap(),
            vec!["claude", "--dangerously-skip-permissions"]
        );
    }

    #[test]
    fn a_later_layer_replaces_the_todo_file() {
        let merged = merge(vec![
            parse("[todo]\nfile = \"/a.md\""),
            parse("[todo]\nfile = \"/b.md\""),
        ]);
        assert_eq!(merged.todo.unwrap().file.unwrap(), "/b.md");
    }

    #[test]
    fn a_later_layer_replaces_the_tickets_provider() {
        let merged = merge(vec![
            parse("[tickets]\nprovider = [\"a\"]"),
            parse("[tickets]\nprovider = [\"b\", \"--x\"]"),
        ]);
        assert_eq!(merged.tickets.unwrap().provider.unwrap(), ["b", "--x"]);
    }

    #[test]
    fn an_absent_key_in_a_later_layer_leaves_the_earlier_value_alone() {
        let merged = merge(vec![
            parse("[agent]\ncommand = [\"claude\"]\n\n[tmux]\nprefix = \"C-Space\""),
            parse("[tmux]\nprefix = \"C-a\""),
        ]);
        assert_eq!(merged.agent.unwrap().command.unwrap(), vec!["claude"]);
        assert_eq!(merged.tmux.unwrap().prefix.unwrap(), "C-a");
    }

    #[test]
    fn pane_arrays_replace_rather_than_append() {
        // Three layers each contributing panes would produce layouts nobody
        // intended, so a layer that defines panes defines the whole layout.
        let merged = merge(vec![
            parse(
                "[[layout.panes]]\nkind = \"builtin\"\nname = \"prs-for-review\"\n\n\
                 [[layout.panes]]\nkind = \"builtin\"\nname = \"branch-status\"",
            ),
            parse("[[layout.panes]]\nkind = \"raw\"\ncommand = [\"lazygit\"]"),
        ]);
        let panes = merged.layout.unwrap().panes.unwrap();
        assert_eq!(panes.len(), 1);
        assert_eq!(panes[0].kind, "raw");
    }

    #[test]
    fn copy_and_hooks_arrays_also_replace() {
        let merged = merge(vec![
            parse("[worktree]\ncopy = [\".env\", \".envrc\"]\nhooks = [\"direnv allow\"]"),
            parse("[worktree]\ncopy = [\".env.local\"]"),
        ]);
        let worktree = merged.worktree.unwrap();
        assert_eq!(worktree.copy.unwrap(), vec![".env.local"]);
        // hooks was absent from the later layer, so the earlier one survives.
        assert_eq!(worktree.hooks.unwrap(), vec!["direnv allow"]);
    }

    #[test]
    fn an_explicitly_empty_array_clears_the_earlier_one() {
        let merged = merge(vec![
            parse("[worktree]\nhooks = [\"direnv allow\"]"),
            parse("[worktree]\nhooks = []"),
        ]);
        assert_eq!(
            merged.worktree.unwrap().hooks.unwrap(),
            Vec::<String>::new()
        );
    }

    #[test]
    fn prs_filter_lists_replace_and_scalars_win() {
        let merged = merge(vec![
            parse("[prs]\ninclude = [\"acme/*\"]\nmax_age = \"30d\""),
            parse("[prs]\ninclude = [\"gameldar/zax\"]"),
        ]);
        let prs = merged.prs.unwrap();
        assert_eq!(prs.include.unwrap(), vec!["gameldar/zax"]);
        // max_age was absent from the later layer, so the earlier one survives.
        assert_eq!(prs.max_age.unwrap(), "30d");
    }

    #[test]
    fn an_empty_prs_list_clears_an_inherited_one() {
        let merged = merge(vec![
            parse("[prs]\nexclude = [\"acme/legacy-api\"]"),
            parse("[prs]\nexclude = []"),
        ]);
        assert_eq!(merged.prs.unwrap().exclude.unwrap(), Vec::<String>::new());
    }

    #[test]
    fn merging_nothing_yields_the_empty_config() {
        assert_eq!(merge(vec![]), RawConfig::default());
    }

    #[test]
    fn an_unknown_key_is_a_parse_error_not_a_silent_ignore() {
        let err = toml::from_str::<RawConfig>("[agent]\ncomand = [\"claude\"]").unwrap_err();
        assert!(err.to_string().contains("comand"), "got: {err}");
    }
}
