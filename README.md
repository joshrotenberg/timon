# timon

One self-contained binary: Lua (and Fennel) over APIs for running Claude and Codex as
subagents, reading past sessions, MCP, git, JSON, HTTP, GitHub and SQLite. Eval from the command
line or write scripts. Skills are bundled; it ships as a Claude Code and Codex plugin.

Design: [docs/design.md](docs/design.md).

## Status

| Phase | Scope | State |
|---|---|---|
| 0 | Design, crate | done |
| 1 | Runtime, directories, skills, REPL, plugin | in progress |
| 2 | `agent`: Claude and Codex runs, contracts, supervisor | planned |
| 3 | `sessions`: list, read and search every session | planned |
| 4 | Toolbox, MCP client and server | to discuss |

## Build

Rust 1.90 or later. Lua is compiled in.

```sh
cargo install --path . --locked
```

## Use

```sh
timon eval '1 + 1'                         # 2
timon eval '{ name = "timon" }'            # {"name":"timon"}
echo 'return ("a,b"):upper()' | timon eval -
timon review.lua 12 14                     # script; args in `arg` and `...`
```

Errors are tables with a kind, a message and a hint; `try(f, ...)` returns `true, ...` or
`false, err`. Globals are strict:

```
$ timon eval 'prnt("hi")'
timon: undefined_global: undefined global "prnt"
  hint: did you mean "print"?
  at eval:1
```

## License

MIT or Apache-2.0, at your option.
