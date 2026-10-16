# Working in timon

`CLAUDE.md` is a symlink to this file.

timon is a task runner in one self-contained Rust binary: tasks and scripts in Lua 5.4 and
Fennel over a toolbox, with agent and session APIs built in. Everything is a task; timon's own
commands are built-in tasks. `docs/design.md` is canonical for direction; `README.md` is canonical for what exists.
Read the design's Rules and API conventions before adding or changing an API.

## Rules

- Branch and open a draft PR before changing anything. Never commit to `main`.
- Before every push: `timon run check`, which runs `cargo fmt --all -- --check`,
  `cargo clippy --all-targets -- -D warnings`, `cargo test`, `RUSTDOCFLAGS=-D warnings cargo
  doc --no-deps` and the plugin check, and exits with the first failure's code. CI runs the
  same gates.
- No em dashes, no AI attribution, conventional-commit prefixes.
- Keep `README.md` and the design's phase table current in the same PR as the change.
- Keep the README and docs tight and terse. Every skill has concrete, runnable examples:
  agents learn the API from examples first. Built-in skills live in `skills/`; every block
  tagged `lua` or `fennel` is run by `tests/skills.rs`, so tag one that needs outside context
  (script arguments, a user's module) `lua fragment`.

## Cadence

While bootstrapping:

- Each unit of work is a GitHub issue with a short What, Done when and Out of scope.
- A PR bundles a few related issues (`Closes #a. Closes #b.`), so work does not wait on one PR
  per issue. Open it as a draft with a short plan, push, and squash-merge on green CI.
- After each issue is implemented, report its status: Done, Next, Blockers, Decisions, and a
  short snippet of what works now.

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

## Dogfood

- `.timon/` is this repository's project layer, and `tests/dogfood.rs` loads and runs it. Use
  it when working on timon, and add to it when a script or agent would help.
- `timon run em-dash` checks our own text for em dashes; `cargo test` runs it over the
  repository.

## State and tests

- Everything timon writes goes under the XDG config, state and cache directories described in
  the design, resolved in one place. `TIMON_HOME` puts all three under one root.
- Tests set `TIMON_HOME` to a temporary directory or pass explicit temporary inputs, and assert
  that no resolved path escapes it. No test reads the real home directory or the real session
  archives.
- Tests never start a real `claude` or `codex`. Provider behavior is tested against recorded
  fixtures; live runs are manual and say so in the PR.
