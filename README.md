# timon

One self-contained binary: Lua (and Fennel) over APIs for running Claude and Codex as
subagents, reading past sessions, MCP, git, JSON, HTTP, GitHub and SQLite. Eval from the command
line or write scripts. Skills are bundled; it ships as a Claude Code and Codex plugin.

Design: [docs/design.md](docs/design.md).

## Status

| Phase | Scope | State |
|---|---|---|
| 0 | Design, crate | done |
| 1 | Runtime, directories, skills, plugin | done (REPL left) |
| 2 | `agent`: runs, contracts, records, budgets, worktrees, events | done |
| 3 | `sessions`: list, read, search and summarize every session | done |
| 4 | Toolbox, MCP client and server | to discuss |

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
