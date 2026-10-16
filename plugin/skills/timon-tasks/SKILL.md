---
name: timon-tasks
description: Use when defining or running timon tasks - a project's or your own tasks.lua, timon tasks, timon NAME, typed options and --help, dependencies and --jobs, task.run, --dry-run, words after --, aliases, groups, confirm and platforms. Load the timon skill first for the basics.
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
    options = {
      base = { type = "string", default = "main", short = "b", doc = "the branch to diff against" },
      model = '("sonnet" | "opus")?',
    },
    run = function(opts, deps)
      return agent.ask{
        prompt = "Review the diff against " .. opts.base .. ". The gates said: " .. deps.check,
        model = opts.model,
      }
    end,
  },
  release = {
    description = "Check, then review against main",
    run = function(opts, deps)
      task.run("check")
      return task.run("review", { base = "main", model = "opus" })
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
timon tasks                   # name, layer, description, by group
timon tasks --all --json      # with private tasks and timon's own commands, options typed
timon check                   # fmt, lint and test, then check; the first failure stops it
timon -j 3 check              # fmt, lint and test at the same time, output prefixed by task
timon c                       # an alias
timon review -b develop --model opus
timon review --help           # the options, from the declaration
timon --dry-run release       # the plan as JSON; nothing runs
timon test -- --nocapture     # words after -- go to the command: cargo test --nocapture
timon task runs               # a task that a timon command shares a name with
timon --yes drop-db           # answers its confirm question
```

## How tasks run

- `run(opts, deps)`: `opts` holds the options and `opts.args` the other words; `deps.NAME` is
  each dependency's first result. A string `run` is a command run without a shell; the words
  are appended as words, so shell syntax in it is refused when tasks load.
- `options` are declared like `cli.parse` options: a type (`"string?"`, `"integer"`,
  `"boolean"`, `"string[]"`, `'"a" | "b"'`) or `{ type, default, short, doc }`. A non-optional
  type without a default must be given. Flags: `--base dev`, `--base=dev`, `-b dev`,
  `--verbose`, `--no-verbose`, a repeated flag for a list. Unknown flags and bad values exit 2
  with did-you-mean, and `timon NAME --help` prints the options.
- Dependencies run once each, depth first in the order declared, then the task, each with its
  own defaults. `--jobs N` (`-j`) runs dependencies that do not depend on each other at the same
  time. The first failure stops the plan, and its exit code is timon's.
- `task.run(name, opts)` runs another task from inside `run`, with its dependencies that have
  not run yet, and returns its result. It checks `opts` like flags and does not ask `confirm`.
- `--dry-run` prints the plan: each task, its level, what it runs and its options. It cannot
  see `task.run` calls inside a function.
- timon's own flags (`--yes`, `--jobs`, `--dry-run`, `--max-usd`, `--resume`) go before the
  task name.
- `confirm` asks before anything runs. Without a terminal it fails unless `--yes` is given.
  `platforms` stops a plan with a task this system cannot run before anything runs.
- A task, its dependencies and their agent runs are one script run: `timon runs --scripts`
  lists it, and `timon --resume ID NAME` reuses its agent runs that succeeded. `budget` limits
  the agent runs inside one task.

## Where tasks come from

- A project's tasks replace yours by name, and load only after a person reviews the project and
  runs `timon trust`. Do not trust a project on someone's behalf.
- timon's own commands take precedence over a task's name. `timon tasks` marks such a task, and
  `timon task NAME` runs it. `timon tasks --all --json` describes timon's commands the way it
  describes tasks: name, description and options with their types.
- Shell completion covers commands, tasks, flags and choices: `eval "$(timon completions zsh)"`
  (or `bash`), or `timon completions fish | source`.
- A script in `scripts/` is a task whose description is its leading comment; it gets its words
  in `arg` and `...`, and can parse them with `cli.parse(arg, options)`. Declare dependencies,
  aliases and the other fields in `tasks.lua`.
- The top level of `tasks.lua` runs whenever tasks are listed, so programs and agents raise
  there: run them inside `run`. Put long task bodies in `lib/` and `require` them.

| Field | Type | Does |
|---|---|---|
| `description` | `string` | one line for `timon tasks` |
| `run` | `function \| string` | `run(opts, deps)`, or a command for `sh` |
| `options` | `table` | typed options: a type string, or `{ type, default, short, doc }` |
| `depends` | `string[]` | tasks to run first, by name |
| `budget` | `{ runs, usd }` | limits on the agent runs inside the task |
| `aliases` | `string[]` | other names on the command line |
| `group` | `string` | a heading in `timon tasks` |
| `private` | `boolean` | left out of `timon tasks` unless `--all`; still runnable |
| `confirm` | `string` | a question answered before anything runs |
| `platforms` | `string[]` | `linux`, `macos`, `windows` and the other Rust OS names |

A release task, with the toolbox:

```lua fragment
release = {
  description = "Bump the version, date the changelog, commit, tag, push, release",
  depends = { "check" },
  options = { version = { type = "string", doc = "the new version" } },
  confirm = "Commit a release?",
  run = function(opts)
    if not git.status().clean then error{ kind = "dirty", message = "commit or stash first" } end
    local old = toml.edit("Cargo.toml", "package.version", opts.version)
    local log = fs.read("CHANGELOG.md")
    fs.write("CHANGELOG.md", (log:gsub("## Unreleased", "## " .. opts.version, 1)))
    git.commit{ message = "chore: release " .. opts.version, all = true }
    git.tag("v" .. opts.version, { message = "release " .. opts.version })
    git.push{ tags = true }
    gh.api("repos/{owner}/{repo}/releases", {
      method = "POST",
      json = { tag_name = "v" .. opts.version, generate_release_notes = true },
    })
    return old .. " -> " .. opts.version
  end,
},
```

In Fennel, `tasks.fnl` returns the same table:

```fennel
;; .timon/tasks.fnl
{:hello {:description "Say hello"
         :run (fn [opts deps] (.. "hello " (or (. opts.args 1) "you")))}}
```
