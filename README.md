# timon

A self-contained agent harness: one binary embedding Lua (and Fennel) over clean APIs for running
Claude and Codex as subagents, reading every past session, calling MCP servers, and working with
git, JSON, HTTP, GitHub and SQLite. Evaluate code from the command line or write scripts. The
skills that teach it are bundled in the binary, and it ships as a plugin for Claude Code and
Codex.

The design is in [docs/design.md](docs/design.md).

## Status

| Phase | Scope | State |
|---|---|---|
| 0 | Design; `timon eval` on plain Lua | in progress |
| 1 | Runtime, directories, bundled skills, plugin | planned |
| 2 | `agent`: Claude and Codex runs, contracts, supervisor | planned |
| 3 | `sessions`: list, read and search every session | planned |
| 4 | Toolbox and MCP client and server | to discuss |

## Build

Requires Rust 1.90 or later. Lua is vendored and compiled in; nothing else is needed to build.

```sh
cargo install --path . --locked
```

## Use

```sh
timon eval '1 + 1'
timon eval 'local t = { name = "timon", phase = 0 }; return t'
echo 'return ("a,b,c"):upper()' | timon eval -
```

A chunk that parses as an expression returns its value. Strings print raw and tables print as
JSON, indented on a terminal.

## License

MIT or Apache-2.0, at your option.
