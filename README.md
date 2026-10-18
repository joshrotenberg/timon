# timon

A task runner in one self-contained binary, in the style of babashka and voga: tasks and
scripts in Lua or Fennel, over a toolbox for the work around a codebase. Agents are built in and
first class: run Claude and Codex as subagents, read every past session, validate answers
against contracts, all with budgets and run records. Skills are bundled, and it ships as a
Claude Code and Codex plugin.

As an agent works, it can offer to keep useful commands, recurring specialist roles and
repeatable collaborations as tested definitions that later sessions discover and reuse. They are
kept when the person agrees; timon is as useful to someone who keeps none.

Design: [docs/design.md](docs/design.md).

## Status

| Phase | Scope | State |
|---|---|---|
| 0 | Design, crate | done |
| 1 | Runtime, directories, skills, plugin | done (REPL left) |
| 2 | `agent`: runs, contracts, records, budgets, worktrees, events; script runs as records, resume | done |
| 3 | `sessions`: list, read, search and summarize every session | done |
| 4 | Toolbox: `sh` and `proc` | done |
| 5 | Tasks: `tasks.lua`, `timon tasks`, `timon NAME`, dependencies | done |
| 6 | Typed options, `--jobs`, `task.run`, `--dry-run`; built-in commands as tasks; completion | done |
| 7 | Toolbox: `git`, `gh`, `http`, `sqlite`, data formats, `eval -i`/`-I`; release tasks | done |
| 8 | Workflow patterns: `flow` with gates, verify, vote, best_of, refine, pipeline, retry, plan | done |
| 9 | MCP: the client (`mcp.connect`, `timon mcp`) and `timon serve` with tasks as tools | done |
| 10 | Hooks: `timon hook`, the session-start brief, the pre-tool guard; memory, queues, schedules | hooks done; the rest to discuss |

## Install

Rust 1.90 or later; Lua and Fennel are compiled in.

```sh
cargo install --locked --git https://github.com/joshrotenberg/timon
```

As a plugin, so agents get the skills and `timon` on the Bash tool's PATH:

```sh
claude plugin marketplace add joshrotenberg/timon
claude plugin install timon@timon
```

In Claude Code: `/plugin install timon --marketplace joshrotenberg/timon`. For Codex:

```sh
codex plugin marketplace add joshrotenberg/timon
codex plugin add timon@timon
```

The plugin runs the installed binary; `TIMON_BIN` points it at another one. It also adds
hooks for both hosts: at session start, a brief of this project's recent sessions, interrupted
runs and tasks; before edits and shell commands, a guard that does nothing until a project
configures it (see [the tasks skill](skills/timon-tasks/SKILL.md#as-hooks)). Codex asks you to
review the hooks first.

In a sandbox that allows writes only to the project, such as Codex's `workspace-write`, timon
needs write access to its state directory (`~/.local/state/timon`: the trust store, run records,
script runs and worktrees) and its cache (`~/.cache/timon`: the session index and summaries).
Add them to Codex's writable roots in `~/.codex/config.toml`, with absolute paths:

```toml
[sandbox_workspace_write]
writable_roots = ["/home/me/.local/state/timon", "/home/me/.cache/timon"]
```

Or point `TIMON_HOME` (or `XDG_STATE_HOME` and `XDG_CACHE_HOME`) at a writable directory. `gh`
calls there need `GH_TOKEN`, since `gh auth token` may not reach its stored credentials. See
[Sandboxed hosts](docs/design.md#sandboxed-hosts).

## Use

```sh
timon eval '1 + 1'                         # 2
timon eval '{ name = "timon" }'            # {"name":"timon"}
echo 'return ("a,b"):upper()' | timon eval -
timon review.lua 12 14                     # script; args in `arg` and `...`
timon eval --fnl '(+ 1 2)'                 # Fennel
timon eval 'json.query({ a = { 1, 2 } }, "a[-1]")'    # JMESPath, with jpx's functions
git log --oneline | timon eval -i 'return #input'       # stdin's lines as input; -I for JSON
timon eval 'toml.edit("Cargo.toml", "package.version", "0.2.0")'   # keeps comments
timon eval 'return git.status().files'     # git's output as data: status, log, diff, tags
timon eval 'return gh.prs{ state = "open" }'                          # GitHub, with GH_TOKEN or gh's login
timon mcp call crates get_crate_info name=mlua   # an MCP server's tool; mcp.connect in Lua
timon serve                                # this project's tasks as MCP tools, over stdio
timon hook < event.json                    # a Claude Code or Codex hook: the brief, the guard, tasks
timon eval 'sh("cargo test")'              # run a program; a failure exits with its code
timon skill show timon                     # the bundled skills
timon eval 'agent.ask{ prompt = "Is 17 prime?", output = "yes_no" }'   # a Claude run, validated
timon eval 'sessions.list{ project = "timon", since = "2d" }'          # past Claude and Codex sessions, timon's runs too
timon sessions search "flaky parser" --since 30d                      # every session's text
```

Tasks live in `tasks.lua` (or `tasks.fnl`) in `~/.config/timon` or a project's `.timon`, and every script in
`scripts/` is a task too:

```lua
return {
  test = "cargo test",
  check = { description = "The gates", depends = { "test" }, run = function() return "ok" end },
}
```

```sh
timon tasks                                # the tasks in reach, with descriptions
timon check                                # test, then check; the first failure stops it
timon test -- --nocapture                  # words after -- go to the task
eval "$(timon completions zsh)"            # completion for commands, tasks and their flags
```

Errors are tables with a kind, a message and a hint; `try(f, ...)` returns `true, ...` or
`false, err`. Globals are strict:

```
$ timon eval 'prnt("hi")'
timon: undefined_global: undefined global "prnt"
  hint: did you mean "print"?
  at eval:1
```

## Working on timon

The repository has its own project layer in `.timon/`: a reviewer, a naming agent and a fixer
agent with their contracts, the gates and release tasks, scripts (em-dash, fix-issues,
naming-audit, parity, recent) and a timon-dev skill. `timon fix-issues 117,118 119` runs the
fixer on each group of issues in its own worktree, then the gates and the reviewer on its branch.
Both came from real work: they were written for a batch of 22 audit issues (#116 to #137), reuse
the reviewer that already existed, and the first run sent a fix back into the script, since the
read-only reviewer could not run `git diff` (#138). The `timon` skill's "Offer to keep what proves
useful" says when to offer a task, script, agent or workflow, and when a one-off stays one.
Read the layer, then trust it once:

```sh
timon trust && timon tasks
timon check              # every gate, before a push
timon em-dash            # also runs in cargo test
timon book               # the doc site, generated: an mdbook in target/book (needs mdbook)
timon update             # install timon from main and update its plugin in Claude Code and Codex
```

## License

MIT or Apache-2.0, at your option.
