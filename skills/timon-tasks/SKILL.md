---
name: timon-tasks
description: Use when defining or running timon tasks - a project's or your own tasks.lua, timon tasks, timon NAME, dependencies, words after --, aliases, groups, confirm and platforms. Load the timon skill first for the basics.
---

# Tasks

A task is a name, a description and something to run. Tasks live in `tasks.lua` (or
`tasks.fnl`) in `~/.config/timon/` and in a project's `.timon/`. Every script in `scripts/` is a
task too.

```lua
-- .timon/tasks.lua
return {
  fmt = "cargo fmt --all",                         -- a command, run with sh
  test = { description = "The tests; words after -- go to cargo test", run = "cargo test" },
  lint = {
    description = "Clippy, with warnings as errors",
    group = "gates",
    run = function(opts, deps)
      sh("cargo clippy --all-targets -- -D warnings")
    end,
  },
  check = {
    description = "Every gate",
    depends = { "fmt", "lint", "test" },
    aliases = { "c" },
    run = function(opts, deps) return "all gates pass" end,
  },
  review = {
    description = "Review the diff against a base branch",
    depends = { "check" },
    budget = { runs = 3, usd = 1 },
    run = function(opts, deps)
      local base = opts.args[1] or "main"
      return agent.ask("Review the diff against " .. base .. ". The gates said: " .. deps.check)
    end,
  },
  ["drop-db"] = {
    description = "Drop the local database",
    confirm = "Drop the local database?",
    platforms = { "linux", "macos" },
    private = true,
    run = "dropdb app_dev",
  },
}
```

```sh
timon tasks                 # name, layer, description, by group; --all adds private tasks
timon check                 # fmt, lint and test, then check; the first failure stops it
timon c                     # an alias
timon review develop        # opts.args is { "develop" }
timon test -- --nocapture   # words after -- are taken as they are: cargo test --nocapture
timon task runs             # a task that a timon command shares a name with
timon --yes drop-db         # answers its confirm question
```

## How tasks run

- `run(opts, deps)`: `opts.args` holds the words after the task name, and `deps.NAME` is each
  dependency's first result. A string `run` is a command for `sh`, with the words appended.
- Dependencies run once each, depth first in the order declared, then the task. The first
  failure stops the plan, and its exit code is timon's.
- Words before `--` that start with `-` are refused until tasks declare options. timon's own
  flags (`--yes`, `--max-usd`, `--resume`) go before the task name.
- `confirm` asks before anything runs. Without a terminal it fails unless `--yes` is given.
  `platforms` stops a plan with a task this system cannot run before anything runs.
- A task, its dependencies and their agent runs are one script run: `timon runs --scripts`
  lists it, and `timon --resume ID NAME` reuses its agent runs that succeeded. `budget` limits
  the agent runs inside one task.

## Where tasks come from

- A project's tasks replace yours by name, and load only after a person reviews the project and
  runs `timon trust`. Do not trust a project on someone's behalf.
- timon's own commands take precedence over a task's name. `timon tasks` marks such a task, and
  `timon task NAME` runs it.
- A script in `scripts/` is a task whose description is its leading comment; it gets its words
  in `arg` and `...`. Declare dependencies, aliases and the other fields in `tasks.lua`.
- The top level of `tasks.lua` runs whenever tasks are listed, so programs and agents raise
  there: run them inside `run`. Put long task bodies in `lib/` and `require` them.

| Field | Type | Does |
|---|---|---|
| `description` | `string` | one line for `timon tasks` |
| `run` | `function \| string` | `run(opts, deps)`, or a command for `sh` |
| `depends` | `string[]` | tasks to run first, by name |
| `budget` | `{ runs, usd }` | limits on the agent runs inside the task |
| `aliases` | `string[]` | other names on the command line |
| `group` | `string` | a heading in `timon tasks` |
| `private` | `boolean` | left out of `timon tasks` unless `--all`; still runnable |
| `confirm` | `string` | a question answered before anything runs |
| `platforms` | `string[]` | `linux`, `macos`, `windows` and the other Rust OS names |

In Fennel, `tasks.fnl` returns the same table:

```fennel
;; .timon/tasks.fnl
{:hello {:description "Say hello"
         :run (fn [opts deps] (.. "hello " (or (. opts.args 1) "you")))}}
```
