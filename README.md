# timon

A task runner in one self-contained binary, in the style of babashka and voga: tasks and
scripts in Lua or Fennel, over a toolbox for the work around a codebase. Agents are built in and
first class: run Claude and Codex as subagents, read every past session, validate answers
against contracts, all with budgets and run records. Skills are bundled, and it ships as a
Claude Code and Codex plugin.

Design: [docs/design.md](docs/design.md).

## Status

| Phase | Scope | State |
|---|---|---|
| 0 | Design, crate | done |
| 1 | Runtime, directories, skills, plugin | done (REPL left) |
| 2 | `agent`: runs, contracts, records, budgets, worktrees, events; script runs as records, resume | done |
| 3 | `sessions`: list, read, search and summarize every session | done |
| 4 | Toolbox: `sh` and `proc` | done |
| 5 | Tasks: `tasks.lua`, `timon tasks`, `timon NAME`, dependencies | done (release tasks wait for `git`) |
| 6 | Typed options, `--jobs`, `task.run`, `--dry-run`; built-in commands as tasks; completion | done |
| 7 | Toolbox: `git`, `gh`, `http`, `sqlite`, data formats, `eval -i`/`-I`; release tasks | done |
| 8 | Workflow patterns | next |
| 9 | MCP client and server | planned |

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

The plugin runs the installed binary; `TIMON_BIN` points it at another one.

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
timon eval 'sh("cargo test")'              # run a program; a failure exits with its code
timon skill show timon                     # the bundled skills
timon eval 'agent.ask{ prompt = "Is 17 prime?", output = "yes_no" }'   # a Claude run, validated
timon eval 'sessions.list{ project = "timon", since = "2d" }'          # past Claude and Codex sessions
timon sessions search "flaky parser" --since 30d                      # every session's text
```

Tasks live in `tasks.lua` in `~/.config/timon` or a project's `.timon`, and every script in
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

The repository has its own project layer in `.timon/`: a reviewer agent, a naming-audit agent
and contract, the gates as tasks, and scripts. Read it, then trust it once:

```sh
timon trust && timon tasks
timon check              # every gate, before a push
timon em-dash            # also runs in cargo test
```

## License

MIT or Apache-2.0, at your option.
