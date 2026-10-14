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
- `acli` on `PATH` and logged in (`acli jira auth login`), for the Jira ticket
  provider. Only needed for the `tickets` pane.
- `zed` on `PATH`, for opening a new worktree. On macOS the CLI ships inside
  the application bundle rather than on `PATH`: open Zed and run `cli: install`
  from the command palette. Set `[editor] command` to use something else, or
  `[]` to open nothing.

`zax doctor` checks tmux, git, gh, gh's auth and the editor, and reports each
separately, so one missing tool never hides the state of the others. Once a
`[tickets] provider` is configured it also starts that provider and reports its
name and protocol. It also reports where the todo file is and how many
items are open, or that there is no file yet.

## Install

With Nix:

```sh
nix build            # ./result/bin/zax
nix run . -- doctor
```

With cargo (toolchain pinned in `rust-toolchain.toml`):

```sh
cargo build --release   # ./target/release/zax and ./target/release/zax-jira
```

The build produces two binaries, `zax` and `zax-jira`. `zax-jira` is the Jira
ticket provider (see [Tickets](#tickets)); put it on `PATH` beside `zax` to
use it as `["zax-jira"]`.

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
| `zax worktree new <branch> [--from <base>] [--no-hooks] [--no-open]` | Branch plus worktree at the canonical path, then copy, hooks and the editor |
| `zax worktree review <pr\|url> [--force] [--no-hooks] [--no-open]` | A pull request's head branch in a worktree, then copy, hooks and the editor |
| `zax worktree list` | The worktrees zax manages for this repository |
| `zax worktree rm <branch> [--force]` | Remove one |
| `zax pane prs-for-review \| branch-status \| my-prs \| tickets \| todo` | Built-in pane processes, normally launched by `session start` |
| `zax pane exec --title <t> --every <dur> -- <cmd>` | Wrap any command's stdout in a header and refresh loop |
| `zax config` | View and edit the three config layers, in a popup over the session. It shows every key, including `editor.command` and `tickets.provider` |
| `zax config show` | The same view, printed: every setting, its effective value and its origin |
| `zax doctor` | Verify tmux, git, gh and the editor are present and authenticated, the ticket provider answers when one is configured, and the todo file is readable |

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
when a worktree is made. Running what the repository says is the point of a
layer whose job is shipping a repository's own layout, but it is worth knowing
before opening an untrusted repository with zax as the agent entry point.

`[tickets] provider` is the same kind of command, with one difference worth
knowing: a provider from any layer, including a repository's own `.zax.toml`,
is run by the shared per-user broker, outside that repository's session. It is
not a child of the pane or the session that named it, it does not inherit the
session's working directory, and it keeps running for as long as any pane in
any session uses it. The values that are not already
"the repository can run commands" are constrained: `name` must be a single
directory name, `tmux.prefix` may not contain a control character, and `copy`
entries may not be absolute or contain `..`.

Unknown keys are errors, not silent defaults.

**All three layers are editable from inside a session.** `prefix C` opens the
config editor over the session - or `zax config` in any pane, which is also the
way in when a config is broken enough that nothing will start. It shows every
setting with its effective value and the layer that supplied it, and writes to
whichever layer you point it at, your own included. Your layer still wins: the
editor changes which file a value lives in, never the precedence between them.

Edits are staged until `s` saves them, and a save validates the whole chain
through the same code a session start uses - an invalid config writes no file at
all. Writes keep the comments and key order of a file you wrote by hand, and a
layer that changed on disk underneath the editor is refused by name rather than
overwritten. What a running session cannot pick up - the prefix, the layout, the
agent command - is reported after the save rather than applied behind your back.

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
size = "55%"

[[layout.panes]]
kind = "builtin"
name = "my-prs"
size = "64%"

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

[editor]
command = ["zed", "-e"]

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
| `layout.panes` | `prs-for-review`, `branch-status` and `my-prs`, at 30%, 55% and 64% | The context panes around the agent |
| `worktree.root` | `~/src/worktrees` | Where worktrees are created |
| `worktree.copy` | none | Gitignored-but-required files to copy into a new worktree |
| `worktree.hooks` | none | Shell scripts run in a new worktree |
| `tmux.prefix` | `C-Space` | Prefix key for zax's tmux server |
| `prs.include` / `prs.exclude` | none | Which repositories `prs-for-review` and `my-prs` show |
| `prs.max_age` | none | Hide pull requests older than this, in `prs-for-review` only |
| `editor.command` | `["zed", "-e"]` | What opens a worktree, with the path as its last argument. `[]` opens nothing |
| `tickets.provider` | none | The ticket provider command, such as `["zax-jira"]`. Without one the `tickets` pane says so and does nothing |
| `todo.file` | `~/.local/share/zax/todo.md` | The markdown file the `todo` pane reads and writes. `$XDG_DATA_HOME` replaces `~/.local/share` |

Durations are written `30s`, `5m`, `2h` or `14d`; a bare number means
seconds.

**A pane size is a percentage of the pane it splits, not of the window.** The
first context pane splits the agent sideways, so its size is the width of the
context column and the agent keeps the rest. Every pane after that splits its
predecessor, so its size is a share of that pane's height and the predecessor
keeps what is left. Sizes compound down the stack and are not meant to add up
to 100: a column of three even rows is written `67%` then `50%`, not `33%`
apiece. The defaults above give 45% / 20% / 35% of a column that is 30% of the
window wide.

### Panes

Three kinds, and the built-ins are privileged instances of the same
mechanism:

- **`builtin`** - a Rust pane shipped with zax. Takes `name`:
  `prs-for-review`, `branch-status`, `my-prs`, `tickets` or `todo`. The
  `tickets` pane is not in the default layout; add it to `[[layout.panes]]`
  once a provider is configured. The `todo` pane is not in the default layout
  either; it needs no config.
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

### Your own pull requests

`my-prs` is the other half of `prs-for-review`: every open pull request you
wrote, in any repository, rather than the ones waiting on your review. It is
in the default layout, as the smaller of the two queues - what other people
are waiting on comes first:

```toml
[[layout.panes]]
kind = "builtin"
name = "my-prs"
size = "64%"
```

Rows are ranked by what is waiting on you, worst first, and ties go to the
older pull request:

| Rank | Means |
| --- | --- |
| conflicting | It no longer merges, and only you can fix that |
| changes requested | A reviewer asked for work |
| checks failing | The build is red |
| unresolved threads | Comments are waiting for an answer |
| ready to merge | Approved, green and mergeable |
| waiting | The reviewers have it; nothing for you |
| draft | Work you parked. It ranks last whatever state it is in |

A row carries two glyphs. The first is the check rollup, exactly as in
`prs-for-review`. The second says what the first cannot:

| Glyph | Means |
| --- | --- |
| `⚠` | Conflicting |
| `✎` | Changes requested |
| `1`-`9`, `+` | That many unresolved review threads |
| `✓` | Ready to merge |
| `·` | Nothing the check glyph is not already saying |

Under the rows, one line spells out all four signals for the selected row -
checks, review decision, threads and mergeability - in words.

`[prs] include` and `exclude` narrow both pull request panes. **`max_age`
applies to `prs-for-review` only.** It exists to hide a review request that
has gone stale; your own three-week-old pull request is not stale, it is the
one that is rotting, and ageing it out of this pane would hide exactly what
the pane is for.

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
| `j` / `k`, arrows | Move the selection (`prs-for-review`, `my-prs`, `tickets`) |
| `w` | `zax worktree review` for the selected row (`prs-for-review`, `my-prs`) |

In the `tickets` pane `enter` and `y` act on the ticket's URL, and the rest
differ:

| Key | Action |
| --- | --- |
| `enter` / `y` | Open / copy the ticket's URL |
| `p` | Open the ticket's most urgent pull request. `no pull request` on the error line when it has none |
| `w` | Start or open work: `zax worktree new` on a branch named from the ticket when it has no worktree, otherwise open its most recently modified worktree in the editor |
| `W` | List the ticket's worktrees in a popup. `j`/`k` move, `d` then `y` removes the selected one with an unforced `git worktree remove` (git refuses one with uncommitted changes; the branch is kept), `esc` closes |
| `s` | Pick a transition: `1`-`9` chooses, `esc` cancels. Absent when the provider does not advertise transitions |

The context panes stay read-only and action-oriented: a keystroke opens, copies
or starts something, and a row action that fails says so on the pane's own error
line rather than looking like a keystroke that did nothing. The one modal in zax
is the config editor, summoned deliberately with `prefix C`:

| Key | Action |
| --- | --- |
| `prefix C` | Open the config editor in a popup over the session |

It needs tmux 3.3 or newer for `display-popup`; on an older tmux `zax doctor`
says so, and `zax config` still works run directly in a pane.

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

## Tickets

`tickets` is the fourth built-in pane: every ticket assigned to you, joined
with the work behind it - worktrees, live agent sessions and pull requests, in
every repository. It needs a ticket provider, a separate executable that
knows the tracker. zax ships one, `zax-jira`, which drives `acli`.

```toml
[tickets]
provider = ["zax-jira"]

[[layout.panes]]
kind = "builtin"
name = "tickets"
size = "30%"
```

A relative path in the provider's program (`bin/jira`) is anchored at the
repository's main worktree, so the same line means the same file wherever the
broker happens to be running from. A leading `~/` is your home directory. A bare name such as `zax-jira` is looked up
on `PATH`. `zax doctor` starts the provider exactly as the broker will and
reports its name and protocol.

```
Tickets (3) Jira         ⟳ 1m
● OMEGA-2713 ▲ ✓  Make exp…
◐ OMEGA-2650 ▲ ✗  Retry st…
○ OMEGA-2702      US124 Ad…
```

Rows are ordered by what you are doing: live work first, then in progress,
then to do, then done. Ties go to the ticket whose pull request needs the
most attention, then to the key. A done ticket is shown only while it still
has work.

| Glyph | Means |
| --- | --- |
| `●` | An agent session is live in a worktree for it. Bold when you are attached to one |
| `◐` | In progress, no live session |
| `○` | To do |
| `✔` | Done, but it still has a worktree or pull request |
| `▲` | A worktree for it exists, in any repository |
| after `▲` | The most urgent pull request, drawn exactly as `my-prs` draws it: the check glyph, then the attention glyph when there is one to add |

The work glyphs go first when the pane is narrow: the title is never squeezed
below 8 columns. Under the rows, one line spells out the selected ticket - its
status, priority, worktrees and pull requests - in words. A ticket is joined
to a worktree or pull request by the key at the front of the branch name, and
to a session by the worktree it runs in.

If `acli` or the provider fails, the last good tickets stay on screen marked
stale, with the reason on the error line.

### The broker

The pane does not run the provider itself. A developer runs several sessions
at once, so one shared, per-user broker owns the provider, polls it once,
caches the result and fans it out to every pane in every session. It also
gathers the work half of the join: the worktrees under `worktree.root`, the
zax sessions, and your pull requests.

- It is started on demand by the first pane that needs it. Two panes racing
  cannot start two brokers.
- It exits 30 seconds after its last pane goes away. A source nobody is
  watching keeps its cache for those 30 seconds too, so a pane restarted by a
  session rebuild shows its tickets at once.
- It polls the provider at the interval the provider advertises as `poll`, or
  every 2 minutes when it advertises none; `zax-jira` advertises `2m`. The
  work half is polled every 60 seconds. `r` in a pane refreshes now.
- Its socket and lock are in `$XDG_RUNTIME_DIR/zax/` when that is set, else
  `~/.cache/zax/`. Its log is `~/.cache/zax/broker.log` (`$XDG_CACHE_HOME`
  replaces `~/.cache`). Look there when a pane shows no tickets.
- It runs with your home directory as its working directory, because it
  outlives the pane that started it and serves every repository.
- A broker restarts only for a newer client: a pane built from a newer `zax`
  binary replaces an older running broker, and a pane never replaces a newer
  one, so two installed builds cannot restart each other in a loop.
- A provider that exits, hangs or answers nothing for 30 seconds is restarted
  with a backoff from one second to five minutes, and the pane keeps showing
  what went wrong with when the next try is. A provider that answers with an
  error, such as `zax-jira` when `acli` is logged out, is not backed off: `r`
  after logging in tries again at once.

### zax-jira

`zax-jira` needs `acli` logged in. It reads
`~/.config/zax/providers/jira.toml` (`$XDG_CONFIG_HOME` replaces `~/.config`),
and works with no file at all:

```toml
jql = "assignee = currentUser() AND statusCategory != Done"
transitions = ["In Progress", "In Review", "Done"]

[projects.OMEGA]
transitions = ["In Progress", "Code Review", "Done"]
```

| Key | Default | Meaning |
| --- | --- | --- |
| `jql` | `assignee = currentUser() AND (statusCategory != Done OR updated >= -14d) ORDER BY updated DESC` | Which tickets to show. The default includes recently done tickets, so their leftover worktrees stay visible |
| `transitions` | `["In Progress", "In Review", "Done"]` | The moves offered by `s` |
| `projects.<KEY>.transitions` | the global list | The moves for one project, which replaces the global list |

`acli` cannot list the transitions a work item allows, so they are configured
here rather than discovered. The ticket's current status is left out of the
list, and each move runs `acli jira workitem transition` with the status name
as its id. A move the workflow does not allow comes back as `acli`'s own error
on the pane's error line. Unknown keys in the file are errors, and a file
that cannot be read is reported on the pane's error line with its path and
line.

## Todo

`todo` is the fifth built-in pane: a todo list you keep by hand, with
optional due dates, shared by every pane in every session.

```toml
[[layout.panes]]
kind = "builtin"
name = "todo"
size = "40%"
```

```
Todo (4 · 1 late)        ⟳ 2s
☐ renew TLS cert       -2d
☐ standup notes      today
☐ review RFC           Fri
☐ call bank         Oct 20
☐ tidy dotfiles
☑ book flights
renew TLS cert · due Mon 5 Oct · 2 days late
```

### The file

The list is a markdown file, `~/.local/share/zax/todo.md` unless `[todo]
file` says otherwise, written as GitHub task-list lines with
[Obsidian Tasks](https://publish.obsidian.md/tasks/) Dataview fields, so it
reads well anywhere and works in Obsidian as it is:

```markdown
# Todo

- [ ] renew TLS cert [due:: 2026-10-10]
- [ ] review RFC on retries
- [x] book flights [due:: 2026-10-06] [completion:: 2026-10-07]
```

- Edit it by hand whenever you like, with the pane open. The pane picks the
  change up within two seconds.
- Any line that is not a task - headings, notes, blank lines - is left
  exactly as it is, and an edit from the pane rewrites only its own line.
- zax reads and writes `due` and `completion`. Any other field, such as
  `[priority:: high]`, is kept as written. A `due` that is not a
  `YYYY-MM-DD` date shows `?` and is quoted on the detail line.
- A file that is a symlink stays one: zax writes the file it points to.

### Rows and keys

Overdue items first (oldest first), then today, then upcoming by date, then
undated. Done items are dimmed and stay until the end of the day they were
checked off; `.` shows every done item in the file.

| Key | Action |
| --- | --- |
| `j` / `k`, arrows | Move the selection |
| `q` / `esc` | Close the pane |
| `r` | Refresh now |
| `a` | Add: type `call bank due:mon` and press `enter` |
| `e` / `enter` | Edit the selected item, prefilled |
| `x` / `space` | Check / uncheck |
| `+` | Snooze: one day later, or tomorrow when it has no due date or is overdue |
| `d` `d` | Delete |
| `o` | Open the file in `[editor] command` |
| `.` | Show / hide older done items |

In the prompt, `due:` takes `today`, `tom`, a weekday (`fri`, the next one),
`+3d`, `+2w` or `2026-10-20`, and is written to the file as a date.
`ctrl-u` clears, `ctrl-w` deletes a word, `esc` cancels.

### Who writes the file

The shared broker does, for every pane, so two panes can never write at once.
An edit names the line it is about as the pane last saw it. If that line
moved or changed - you saved the file by hand meanwhile - the edit is
refused with `the todo file changed - refreshed` rather than applied to the
wrong line, and the pane shows the file as it is now. It is refused the same
way if the file changes during both write attempts. Any other hand edit
leaves the line alone, and the edit is applied on top of it.

## Writing a ticket provider

A provider is any executable that speaks protocol version 1 on stdin and
stdout, so a tracker other than Jira is another binary and a line of config.
Every message is one line of JSON-RPC 2.0. The broker sends five methods:

| Method | Params | Result |
| --- | --- | --- |
| `initialize` | `{protocol}` | `{protocol, name, capabilities: {transitions}, poll?}` |
| `tickets/assigned` | `{}` | `{tickets: [{key, summary, status: {category, name}, priority?, kind?, url}]}` |
| `tickets/transitions` | `{key}` | `{transitions: [{id, name}]}` |
| `tickets/transition` | `{key, id}` | `{}` |
| `shutdown` | `{}` | `{}`, after which the broker closes stdin |

- `status.category` is one of `to_do`, `in_progress` or `done`; it drives
  ordering and colour, while `status.name` is shown as the tracker calls it.
  A ticket with a category zax does not know is dropped, not the list.
- `poll` is how often to be asked for tickets, such as `30s`, `5m` or `2h`.
- The two transition methods are used only when `capabilities.transitions` is
  true. A transition `id` is opaque to zax and is handed back unchanged.
- A provider that answers `initialize` with a different protocol is refused.
- An error carries `data: {"kind": ...}`, one of `not_authenticated`,
  `missing_tool`, `unavailable`, `invalid_request` or `unsupported`. The
  message is shown on the pane's error line, so make it say what to do.
- A provider may send the notification `tickets/changed` to say "poll now".
  The broker currently accepts it and acts on it at the next poll.

Write diagnostics to stderr, which the broker logs; stdout is the protocol.
`crates/zax-protocol/src/tickets.rs` is the reference for the payloads, and
`rpc.rs` beside it for the envelope and the error kinds. A Rust provider needs
only that crate.

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

Last, the finished worktree is opened in `[editor] command`, which defaults to
`zed -e` and takes the worktree's path as its final argument. This happens
after the copies and the hooks, so what opens is a worktree that is ready to
work in rather than one that fills itself in while you look at it. `w` in the
pull request panes goes through `zax worktree review`, so a review ends in an
editor window on the branch.

Pass `--no-open` to skip it for one command, or set `command = []` to never
open anything. An editor that will not start is a warning and not a failed
command: the worktree was made either way. A pane that pressed `w` shows that
warning in its header, because from there a worktree with no editor window on
it looks exactly like a keystroke that did nothing. The command is run with
its output captured and is waited on, which suits an editor that hands the
path to a running application and returns - `zed`, `code`, `nvim --remote` -
rather than a terminal editor expecting this terminal. The editor is the one
program zax starts that keeps its own way of asking the user a question;
everything else zax runs is told not to ask.

### Credentials

zax captures the output of every tool it runs, so no tool it runs may ask a
question on the terminal: that terminal belongs to a pane's interface, which
would paint over the question and read the keystrokes meant for it. A prompt
there is not a question but a hang. So `GIT_TERMINAL_PROMPT=0` and
`SSH_ASKPASS_REQUIRE=force` move every question off it.

Off the terminal is not the same as unanswerable, and a locked ssh key is the
case that makes the difference. Moving that question somewhere it can still be
answered means an askpass helper, so zax uses the one you have: `$SSH_ASKPASS`
if you set it, otherwise whichever of the usual packaged paths exists
(`/usr/lib/openssh/ssh-askpass` and friends) as long as `$DISPLAY` or
`$WAYLAND_DISPLAY` says there is somewhere to draw it. A passphrase then
arrives as a dialog, and `w` works on a cold boot.

With no helper and no display - over plain ssh, say - there is nothing to ask
through, and the fetch fails rather than hanging: `Permission denied
(publickey)` in the pane header instead of a command that never returns. An
agent holding the key avoids the question altogether, which is the other way
to arrive at a working `w`.

`git`'s own credential prompts are not searched for a helper. Those have a
real answer in a credential helper - `gh auth setup-git` writes one - and a
dialog zax went looking for would be answering a question that should not have
been asked. An explicit `$GIT_ASKPASS` is still respected.

`zax doctor` checks the agent whenever `origin` is reached over ssh, telling an
agent that holds no keys apart from no agent at all; an https remote is never
asked about keys, because a token is what it needs. GNOME's `gcr-ssh-agent` is
a known offender behind an agent that cannot be reached, and pointing
`IdentityAgent` at the real agent it proxies is the usual cure:

```sshconfig
Host github.com
  IdentityAgent /run/user/1000/gcr/.ssh
  AddKeysToAgent yes
```

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

zax-protocol  wire types for the broker and providers (serde, serde_json)
zax-broker    shared per-user broker: socket, sources (tickets, work, todo), fan-out
              (zax-domain, zax-protocol)
zax-jira      the Jira ticket provider, a separate binary
              (zax-exec, zax-protocol)
```

`zax-protocol` depends on nothing else in the workspace, so a provider written
in Rust needs only it. `zax-broker` and `zax-jira` sit beside the main stack
rather than in it: the broker depends only on `zax-domain` and `zax-protocol`,
and `zax-jira` only on `zax-exec` and `zax-protocol`. Nothing at `zax-app` or
below mentions tmux, git or GitHub by name. Parsing happens once, at the boundary, into types that make the invalid states
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
