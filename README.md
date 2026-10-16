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
| 5 | Tasks: `tasks.lua`, `timon tasks`, `timon <task>` | next |
| 6 | Built-in tasks, typed options, plans | planned |
| 7 | Toolbox: `git`, `gh`, `http`, `sqlite` | planned |
| 8 | Workflow patterns | planned |
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
timon run triage                           # a named script from ~/.config/timon or .timon
timon eval 'sh("cargo test")'              # run a program; a failure exits with its code
timon skill show timon                     # the bundled skills
timon eval 'agent.ask{ prompt = "Is 17 prime?", output = "yes_no" }'   # a Claude run, validated
timon eval 'sessions.list{ project = "timon", since = "2d" }'          # past Claude and Codex sessions
timon sessions search "flaky parser" --since 30d                      # every session's text
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
and contract, and scripts. Read it, then trust it once:

```sh
timon trust && timon scripts
timon run em-dash        # also runs in cargo test
```

## License

MIT or Apache-2.0, at your option.
