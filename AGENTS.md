# Working in timon

`CLAUDE.md` is a symlink to this file.

timon is one self-contained Rust binary embedding Lua 5.4 and Fennel over agent, session and tool
APIs. `docs/design.md` is canonical for direction; `README.md` is canonical for what exists.
Read the design's Rules and API conventions before adding or changing an API.

## Rules

- Branch and open a draft PR before changing anything. Never commit to `main`.
- Before every push: `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`,
  `cargo test`, and `RUSTDOCFLAGS=-D warnings cargo doc --no-deps`.
- Hosted CI cannot start in this private repository while the account's Actions billing is
  blocked. The local gates above are the gates; say so in the PR when CI did not run.
- No em dashes, no AI attribution, conventional-commit prefixes.
- Keep `README.md` and the design's phase table current in the same PR as the change.

## Self-contained

- No path, git or workspace dependency on a sibling project (trovador, due, tosi, bottega and the
  rest). Code that is needed is copied in. The commit that copies it names the source repository,
  path and commit.
- Libraries from crates.io are fine when they are libraries for the job (mlua, rusqlite,
  tower-mcp, claude-wrapper, codex-wrapper). Pin the ones whose behavior the runtime depends on.
- At run time timon may start only `claude`, `codex` and `git`, plus whatever a script asks for
  explicitly. No other tool may be required.

## Language

- Lua and Fennel are used as published. No custom syntax, no core macros, no data-structure
  library, no type system beyond LuaLS stubs. If a change needs a language feature, change the
  API instead.
- Lua is the canonical language for built-ins, skills and examples. Fennel examples are
  additions, never replacements.
- Nothing essential may depend on a Lua 5.4-only feature. `<close>` stays shorthand for
  `scope(fn)`, so the Lua version remains a cargo feature choice.
- Every options table is declared, and the declaration drives strict checking, `help()`, the
  LuaLS stubs and the skill references. Never add an option without declaring it.

## State and tests

- Everything timon writes goes under the XDG config, state and cache directories described in
  the design, resolved in one place. `TIMON_HOME` puts all three under one root.
- Tests set `TIMON_HOME` to a temporary directory or pass explicit temporary inputs, and assert
  that no resolved path escapes it. No test reads the real home directory or the real session
  archives.
- Tests never start a real `claude` or `codex`. Provider behavior is tested against recorded
  fixtures; live runs are manual and say so in the PR.
