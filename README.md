# zax - Zed Agent eXtensions

Zed's AI agent panel runs a single command when it opens a terminal thread
(`agent.terminal_init_command`, usually `claude`). That terminal is where
worktree-based development actually happens, and it shows nothing but the
agent.

`zax` takes over that entry point. It builds a tmux layout in which the agent
runs unchanged in the main pane, surrounded by glanceable context panes -
pull requests waiting on your review, the status of this worktree's own pull
request, or anything else you can express as a command - and it ships the
worktree utilities that Zed tasks can call from Zed's own menus.

It runs on its own tmux socket with its own configuration, so it can neither
read nor disturb a personal tmux setup, and its sessions outlive Zed: closing
the agent terminal or restarting Zed reattaches to a live agent with its
scrollback intact.

## Requirements

- `tmux`, `git` and `gh` on `PATH`.
- `gh` already authenticated. zax never manages GitHub auth; it inherits it.

`zax doctor` checks tmux, git, gh and gh's auth, and reports each separately,
so one missing tool never hides the state of the others.

## Install

With Nix:

```sh
nix build            # ./result/bin/zax
nix run . -- doctor
```

With cargo (toolchain pinned in `rust-toolchain.toml`):

```sh
cargo build --release   # ./target/release/zax
```

## Quick start

Point Zed's agent terminal at zax. In Zed's `settings.json`:

```json
{
  "agent": {
    "terminal_init_command": "zax session start"
  }
}
```

Open the agent panel in any git repository or worktree. zax derives the
session from the repository and branch, reattaches to one nobody is looking
at or builds a new layout, and attaches. Run it from a terminal too, if you like:

```sh
cd ~/src/acme
zax session start
```

`zax` refuses to start inside an existing multiplexer: if `$TMUX` or
`$TMUX_PANE` is set it says so and exits rather than nesting tmux inside tmux.

The agent pane runs with `$TMUX` removed from its environment. Claude Code
stops animating its terminal title when it sees `$TMUX`, and that animated
title is what drives Zed's in-progress indicator for a thread. tmux's own
`$TMUX_PANE` stays set, which is why the check above looks at both.

## Commands

| Command | Purpose |
| --- | --- |
| `zax session start` | Reattach to a detached session in this worktree, or build a new one |
| `zax session list` | The zax sessions on the dedicated socket |
| `zax session kill [name]` | Kill a session. Defaults to this worktree's own, when it has only one |
| `zax worktree new <branch> [--from <base>] [--no-hooks]` | Branch plus worktree at the canonical path, then copy and hooks |
| `zax worktree review <pr\|url> [--force] [--no-hooks]` | A pull request's head branch in a worktree, then copy and hooks |
| `zax worktree list` | The worktrees zax manages for this repository |
| `zax worktree rm <branch> [--force]` | Remove one |
| `zax pane prs-for-review \| branch-status` | Built-in pane processes, normally launched by `session start` |
| `zax pane exec --title <t> --every <dur> -- <cmd>` | Wrap any command's stdout in a header and refresh loop |
| `zax doctor` | Verify tmux, git and gh are present and authenticated |

## Configuration

Three layers, each overriding the previous:

| # | Path | What it is |
| --- | --- | --- |
| 1 | `~/.config/zax/config.toml` | Global defaults |
| 2 | `<repo>/.zax.toml` | What the repository ships, checked in |
| 3 | `~/.config/zax/repos/<repo>/config.toml` | Your personal override for that repository |

`$XDG_CONFIG_HOME` replaces `~/.config` when it is set. A layer whose file is
absent is an empty layer, not an error. Layer 3 is keyed on the directory
name of the main working tree; a `name` key in layer 2 changes that key, for
the rare case of two repositories with the same directory name.

**Your layer wins.** A repository can suggest a layout; it can never impose
one, and tuning a repository you do not own never dirties its working tree.

**Arrays replace, they do not append.** A layer that defines
`[[layout.panes]]` defines the whole layout - three layers each contributing
panes would produce layouts nobody intended. The same goes for `copy`,
`hooks`, and the `[prs]` lists. An explicitly empty array clears an inherited
one.

**Layer 2 is a trust boundary.** It is a file checked into somebody else's
repository, and zax runs what it says: `raw` and `exec` pane commands are
executed when a session starts, and `[worktree] hooks` run through `sh -c`
when a worktree is made. That is the point of a layer whose job is shipping a
repository's own layout, but it is worth knowing before opening an untrusted
repository with zax as the agent entry point. The values that are not already
"the repository can run commands" are constrained: `name` must be a single
directory name, `tmux.prefix` may not contain a control character, and `copy`
entries may not be absolute or contain `..`.

Unknown keys are errors, not silent defaults.

### A full example

```toml
[agent]
command = ["claude"]

[[layout.panes]]
kind = "builtin"
name = "prs-for-review"
size = "30%"

[[layout.panes]]
kind = "builtin"
name = "branch-status"
size = "25%"

[[layout.panes]]
kind = "exec"
title = "Deploys"
every = "60s"
command = ["kubectl", "get", "pods", "-n", "prod"]

[[layout.panes]]
kind = "raw"
command = ["lazygit"]

[worktree]
root  = "~/src/worktrees"
copy  = [".env", ".envrc"]
hooks = ["direnv allow", "bundle install"]

[tmux]
prefix = "C-Space"

[prs]
include = ["acme/*", "gameldar/zax"]
exclude = ["acme/legacy-api"]
max_age = "14d"
```

### Every key

| Key | Default | Meaning |
| --- | --- | --- |
| `agent.command` | `["claude"]` | What runs in pane 0 |
| `layout.panes` | `prs-for-review` at 30%, `branch-status` at 25% | The context panes around the agent |
| `worktree.root` | `~/src/worktrees` | Where worktrees are created |
| `worktree.copy` | none | Gitignored-but-required files to copy into a new worktree |
| `worktree.hooks` | none | Shell scripts run in a new worktree |
| `tmux.prefix` | `C-Space` | Prefix key for zax's tmux server |
| `prs.include` / `prs.exclude` | none | Which repositories `prs-for-review` shows |
| `prs.max_age` | none | Hide pull requests older than this |

Durations are written `30s`, `5m`, `2h` or `14d`; a bare number means
seconds. Pane sizes are percentages, with or without the `%`, and must leave
room for the agent.

### Panes

Three kinds, and the built-ins are privileged instances of the same
mechanism:

- **`builtin`** - a Rust pane shipped with zax. Takes `name`:
  `prs-for-review` or `branch-status`.
- **`exec`** - runs a command on a loop and wraps its stdout in a styled
  header with a last-updated time, stale and error marking, and scrollback.
  Takes `title`, `every` and `command`.
- **`raw`** - the command owns the pane and zax gets out of the way. This is
  the answer to "I want a full TUI for X": run `gh-dash` or `lazygit` here
  rather than having zax reimplement it. Takes `command`.

Every pane polls on its own interval with jitter and refreshes when it gains
focus. It keeps the last good result and marks itself stale on failure rather
than blanking, because a `gh` hiccup must never look like an empty review
queue.

### Filtering the review queue

`prs-for-review` shows every pull request that asks for your review, which on
a busy account is more than a narrow pane can usefully hold. `[prs]` narrows
it:

```toml
[prs]
include = ["acme/*", "gameldar/zax"]
exclude = ["acme/legacy-api"]
max_age = "14d"
```

- `include` and `exclude` are lists of `owner/name`, or `owner/*` for every
  repository of one owner. A `*` anywhere else is an error rather than a
  prefix match.
- An absent or empty `include` means every repository.
- An exclusion beats an inclusion, so the example above reads the way it
  looks: everything of `acme`, except `legacy-api`.
- `max_age` hides anything opened longer ago than it.

zax also folds what it can into the GitHub search itself, so the search's
50-result cap applies to an already narrowed queue rather than to everything.
Only what translates exactly is sent, and the search is only ever allowed to
be looser than the filter, so what you see is always what the config asks
for.

## Keys in a session

tmux's prefix is `C-Space` by default. Pane 0 is the agent and should swallow
as few keystrokes as possible, so pane switching is also bound unprefixed:

| Key | Action |
| --- | --- |
| `Alt+h` / `Alt+l` | Focus the pane left / right |
| `Alt+k` / `Alt+j` | Focus the pane up / down |

Inside a focused built-in pane:

| Key | Action |
| --- | --- |
| `enter` | Open the pull request in a browser |
| `y` | Copy its URL |
| `r` | Refresh now |
| `q` / `esc` | Close the pane |
| `j` / `k`, arrows | Move the selection (`prs-for-review`) |
| `w` | `zax worktree review` for the selected row (`prs-for-review`) |

There is no modal UI and there are no forms. A row action that fails says so
on the pane's own error line rather than looking like a keystroke that did
nothing.

zax renders its own tmux configuration to `~/.cache/zax/tmux.conf`
(`$XDG_CACHE_HOME` replaces `~/.cache` when set) on every `session start`.
Nothing else reads it and it is safe to delete.

**The prefix is a property of the zax server, not of a repository.** tmux
reads its configuration only when it starts a server, and every zax session
shares the one `-L zax` socket, so the first session to start that server
fixes the prefix for every repository until it exits. A second repository's
`[tmux] prefix` is read and merged as usual but changes nothing while a
server is already running. To pick up a new prefix, stop every zax session or
run `tmux -L zax kill-server`.

## Worktrees

zax puts worktrees at `<worktree.root>/<repository>/<branch>`, never inside
the repository itself, and the rule is a type rather than a convention: an
invalid worktree location cannot be constructed.

```sh
zax worktree new OMEGA-1963-export-csv-reports --from origin/main
zax worktree list
zax worktree rm OMEGA-1963-export-csv-reports
```

Branches you create are held to the project's naming rule:
`<TICKET>-<short-desc>`, or a bare lowercase topic, with no `feat/`-style
category prefix and no slash. `zax worktree review` is the one place that
bends, because a pull request's head branch is not yours to name: it keeps
whatever the branch is called, and a slash in it becomes a real directory
separator.

After creating a worktree, `worktree.copy` copies the
gitignored-but-required files that are the usual reason a fresh worktree is
not immediately usable, and `worktree.hooks` run in it through `sh -c` with:

| Variable | Value |
| --- | --- |
| `ZAX_WORKTREE` | The new worktree's path |
| `ZAX_BRANCH` | The branch name |
| `ZAX_TICKET` | The ticket key, when the branch carries one |
| `ZAX_SOURCE_REPO` | The main working tree's path |

Pass `--no-hooks` to skip the hooks. The copies still happen; a file listed
in `copy` but absent from the source tree is skipped rather than being an
error, so a repository config can list `.envrc` for the people who use
direnv.

## Sessions

Sessions are named `zax-<repo>-<branch>-<fingerprint>` and live on the
dedicated `-L zax` socket. The fingerprint is what keeps two worktrees whose
names sanitise alike from sharing one, so `zax session kill` can never tear
down the wrong agent.

A worktree can run several agents side by side. `zax session start` reattaches
to the lowest-numbered session in the worktree that nobody is attached to, and
only when every one is attached does it build another, suffixed `-2`, `-3` and
so on (up to 99). A second agent thread in Zed therefore gets its own agent,
while closing an agent terminal or restarting Zed still reattaches to the live
agent you left.

```sh
zax session list                       # attached sessions are marked
zax session kill                       # this worktree's own, if it has one
zax session kill zax-acme-topic-1a2b3c4d-2
```

Killing one session does not tear down the server other worktrees are using.

## Layout of the repository

A cargo workspace, so the inward-pointing dependency rule is a compile error
rather than a convention:

```
zax-domain   pure types, no I/O
   ^
zax-app      use cases + ports (traits)
   ^
zax-config  zax-infra-tmux  zax-infra-git  zax-infra-github  zax-tui
   ^
zax-cli      clap + composition root
```

Nothing at `zax-app` or below mentions tmux, git or GitHub by name. Parsing
happens once, at the boundary, into types that make the invalid states
unrepresentable.

The original design document is
`docs/superpowers/specs/2026-09-18-zax-design.md`.

## Development

```sh
nix develop          # rust, tmux, gh, git, cargo-nextest, rust-analyzer

cargo test --workspace
cargo clippy --workspace --all-targets
cargo fmt --all
```

`nix flake check` runs fmt, clippy and the tests. Warnings are errors. The
integration tests boot a real tmux server on a temporary socket, so tmux has
to be present to run the full suite.
