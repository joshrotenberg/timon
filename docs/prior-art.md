# Prior art

How timon's features map to the tools it learns from. Checked 2026-10-16 against each tool's
documentation on that date: babashka 1.13.226 (through voga's bb-playground comparison), voga's
main branch, just 1.46 or later (the `[arg]` attribute), Task, mise, GNU make, Dagger, Subtask
and ai-dispatch 10.6.0. No benchmarks were run. The design's short version is in
[design.md](design.md#prior-art).

In timon's column, **done** means built and tested here, **P10** is the phase that delivers it
(see the design's Phases), `#N` is the issue, and **not planned** is not on the
roadmap. In the other columns, a feature is named as the tool names it. **no** means the tool's
documentation for that surface was checked and does not have it; **not checked** means it was
not. A tool whose language could implement a feature through a library still gets **no** when
the tool does not ship it.

## How to read the comparisons

| Tools | What they measure |
|---|---|
| just, Task, mise, make | the bar for ordinary repository commands: how little it takes to declare and run one |
| babashka | the whole environment: a scripting language, a task runner and a toolbox in one binary |
| voga | the same model as timon on another language (Rhai), with retained runs |
| Lark, Lune, Flick, tal | whether "Lua plus a task runner in one binary" is new; it is not |
| Dagger | a heavier execution model: containers and a content-addressed cache, given up on purpose |
| Subtask, ai-dispatch | the value of agent-specific integration on its own |

The harder alternative to timon is babashka plus a small library for agents, not babashka
alone. timon has to earn its place through the whole experience: one install, stock Lua and
Fennel, the toolbox, and agent and session APIs that share records, budgets and options with
everything else.

## Tasks

| Feature | timon | babashka | voga | just | Task | mise | make |
|---|---|---|---|---|---|---|---|
| Where tasks live | done: `tasks.lua`, `scripts/` | `bb.edn` | `voga.toml`, naming `.rhai` files | `justfile` | `Taskfile.yml` | `mise.toml`, `mise-tasks/` | `Makefile` |
| Language | Lua 5.4, Fennel | Clojure (SCI) | Rhai | just syntax over a shell; shebang recipes in any language | YAML over a shell | a shell; file tasks in any language | make over a shell |
| List tasks with descriptions | done | `bb tasks` | `voga tasks` | `just --list` | `task --list` | `mise tasks` | no |
| Run as `tool NAME` | done | yes | yes; built-ins win | yes | yes | `mise run NAME` | yes |
| Dependencies, each run once | done | `:depends` | `depends` | yes | `deps` | `depends`, `depends_post`, `wait_for` | prerequisites |
| A dependency's result goes to the task | done | yes | yes | no | no | no | no |
| Independent dependencies in parallel | done: `--jobs` | `run --parallel` | `--jobs` | `[parallel]` | concurrent, with limits | `jobs` | `-j` |
| Call a task from code | done: `task.run` | `(run 'task)` | `task::run` | recursive `just` | a task in `cmds` | recursive `mise run` | recursive `$(MAKE)` |
| Typed options and generated `--help` | done | `:exec-fn` with a babashka.cli `:spec` | yes | parameters, `[arg]` flags | variables with required and allowed values | `usage` specs | variables |
| Subcommands of a task | no | `:cmd` | no | no | not checked | not checked | no |
| Pass arguments through | done: words, and everything after `--` | `*command-line-args*` | `passthrough` | variadic parameters | after `--` | `raw_args` | no |
| Exit status chosen by the task | done: `error{ exit = N }` | `:babashka/exit` | `exit` | the command's | the command's | the command's | the command's |
| Dry run | done: the plan as JSON | no | `--dry-run` | `--dry-run` | a preview of the commands | `--dry-run` | `-n` |
| User and project layers | done | project only | user layer, `promote` | `--global-justfile` | not checked | global config | `include` |
| The tool's own commands described like tasks | done: `timon tasks --all --json` | no | no | no | no | `list_commands` over MCP | no |
| Aliases | done | no | `aliases` | `alias` | `aliases` | `alias` | no |
| Groups in the listing | done | no | `group` | `[group]` | namespaces from includes | name prefixes (`lint:x`) | no |
| Private tasks | done | `:private` | `private` | `[private]` | not checked | `hide` | no |
| Confirm before running | done; fails without a terminal unless `--yes` | no | `confirm` | `[confirm]` | prompts | `confirm` | no |
| Platform-specific tasks | done | no | `platforms` | `[linux]`, `[macos]` and others | platform-specific commands | `run_windows` | no |
| Interactive picker | not planned | no | `voga choose` | `--choose` | not checked | `mise run` with no task | no |
| Shell completion | done: from the declarations | from `:exec-fn` and `:cmd` declarations | yes | yes | not checked | yes | from the shell |
| Skip work that is up to date | `fs.newer` in #73 | `fs/modified-since` | `fs::modified_since` | `[cache]`, unstable | up-to-date checks on files | `sources`, `outputs` | file times |
| Cleanup after a task | no | not checked | no | no | `defer` | `depends_post` | no |
| Watch files | later, to discuss | no | schedules instead | no | watch mode | `mise watch` | no |
| Load `.env` | later, to discuss | no | no | `dotenv-load` | not checked | `env` | no |
| Remote tasks or packages | not planned | `:deps` | no | no | remote Taskfiles | remote `file`, includes | no |

Lua task runners exist already. Lark is a Lua environment for build and source-tree work, with
parameterized and pattern-matched tasks and explicit parallel groups; it is pre-1.0. Lune is a
standalone Luau runtime written in Rust, with filesystem, networking and stdio libraries; it is
a toolbox and runtime, not a task runner, and runs Luau rather than stock Lua. Flick's
`flick.lua` returns a table whose values are shell strings or Lua functions, run on LuaJIT;
timon took its command-string shorthand. tal runs `.task.lua` files inside the `run` script
manager, with incremental builds keyed on SHA-256.

## Runtime and toolbox

just, Task, mise and make run shell commands, so their toolbox is whatever the shell finds.

| Feature | timon | babashka | voga |
|---|---|---|---|
| REPL | #11 | REPL and nREPL | no |
| Evaluate an expression | done: `eval` | `-e` | `voga eval` |
| Read stdin as data | done: `-i`, `-I`, `-o`, `-O`, `--stream` | `-i`, `-I`, `-o`, `-O`, `--stream` | no |
| Shebang scripts | done | yes | no |
| Concurrency in the language | done: `scope`, `spawn`, `par` | futures, `pmap`, core.async | `process::start`, `task::run` with `jobs` |
| Run a program, raise on failure | done: `sh` | `shell` | no; results are data |
| Run a program, result as data | done: `proc.run` | `process` | `process::run` |
| Background processes | done: `proc.spawn` | `process` | `process::start` |
| Process groups, timeouts, cleanup | done | `destroy-tree`, by hand | yes |
| Pipelines | #76 | `pipeline` | `process::pipeline` |
| Files and paths | done; more in #73 | `babashka.fs` | `fs` |
| JSON and queries | done: `json`, JMESPath in process (jpx) | cheshire, Clojure's functions | `json`, JMESPath |
| TOML, YAML, CSV | done: `toml` (with `set` and `edit` that keep every other byte), `yaml`, `csv`, `markdown` | YAML, CSV, EDN, Transit | TOML, CSV; YAML planned |
| HTTP client | done: `http`, cancellable, with timeouts | `babashka.http-client` | `http` |
| git | done: `git`, output parsed into data, worktrees | through `shell` | `voga/git`, read only |
| GitHub | done: `gh`, REST and GraphQL | through `shell` | `voga/gh` |
| SQLite | done: `sqlite`, bundled | a pod | no |
| Edit files | `toml.edit` for TOML; the rest in #51, a maybe | `spit` | `edit` |
| Parse a script's arguments | done: `cli.parse`, the task options parser | `babashka.cli` | task options only |

## Runs

| Feature | timon | babashka | voga | mise | Dagger |
|---|---|---|---|---|---|
| A record of each run | done: script runs and agent runs | no | yes, journaled | no | traces |
| Reuse results on a rerun | done: `--resume` reuses agent runs that succeeded and can be reused; nothing else is replayed | no | no; an interrupted run is marked uncertain | `sources`, `outputs`; a cache, experimental | content-addressed cache |
| Cancellation reaches child processes | done | by hand | yes | not checked | containers |
| Spending limits | done: run counts checked before each run starts; USD checked against what finished runs reported | no | no | no | no |
| Progress output | done: `--progress`, `prefix` | no | no | `output` modes | a TUI |
| Schedules | P10 (#35) | no | yes | no | no |
| Long-running services | not planned | no | yes, with readiness | `daemons`, experimental | services |
| Sandboxing | not planned; trust is per project, strict globals are not a sandbox, and the hook guard checks what an agent asks for, not what a process does | no | project-local writes | `deny_*`, `allow_*` | containers |

## Agents

| Feature | timon | voga | Dagger | Subtask | ai-dispatch |
|---|---|---|---|---|---|
| How agents run | done: the Claude Code and Codex CLIs | an example task over processes | an `LLM` type calling model APIs | a Claude Code skill and CLI | agent CLIs behind one interface |
| Agents supported | Claude Code, Codex | any CLI | model providers | Claude Code, Codex | claude, codex, gemini, copilot, opencode, cursor and others |
| Answers validated against a shape | done: contracts | no | typed environment outputs | not documented | not documented |
| A worktree per run | done: `worktree = true` | no | containers instead | yes, per task | `--worktree` |
| Parallel and background runs | done: `spawn`, `par` | `--jobs` | engine concurrency | yes | `--bg`, `batch --parallel` |
| Dependencies between agent tasks | done: in code and as tasks | `depends` | in code | no | `depends_on` in batch files |
| Records and logs | done | run records | traces | progress tracking | JSONL logs, SQLite |
| Cost reporting | done: per run and per script; Codex reports none, so its USD is unknown | no | not documented | not documented | `aid usage` |
| Enforced limits | done: runs and USD, nested by scope | no | no | no | budgets in config |
| Continue with feedback | done: `agent.continue` | no | prompts build on each other | request changes | `aid retry --feedback` |
| Verify, review, best of N | done: `flow` (check, verify, vote, best_of, refine, pipeline, retry, plan); more in #99 | no | an agent loop in code | a review step | `--verify`, `--peer-review`, `--best-of` |
| Resume a workflow from its runs | done: `--resume` | no | the cache | not documented | not documented |
| Search past sessions | done: `sessions` | no | no | no | no |
| Teach the host agent | done: a plugin for Claude Code and Codex | built-in skills | no | a Claude Code skill | a prompt for `CLAUDE.md` |
| Host hooks | done: `timon hook` for both hosts, a session-start brief, a pre-tool guard, tasks as handlers | not checked | not checked | not checked | not checked |
| Memory | P10 | no | no | no | `aid memory` |

## Extension and MCP

| Feature | timon | babashka | voga | Dagger | mise | ai-dispatch |
|---|---|---|---|---|---|---|
| Call out-of-process tools | done: `mcp.connect`, the MCP client | pods | `mcp::connect` | `LLM.withMCPServer` | no | no |
| Serve tasks over MCP | done: `timon serve` | no | `voga mcp` | modules as an MCP server | `run_task`, and tasks as a resource | `aid mcp` |
| Declared effects of a command | #83 | no | no | no | `list_commands` marks read, write or destructive | no |
| Bundled docs for agents, with tested examples | done: skills | no | skills | no | no | no |

Serving tasks to agents over MCP is not new: voga, mise, Dagger and ai-dispatch do it.

## timon's intended combination

The tools above overlap with timon in every row. What timon puts together:

- Ordinary tasks and agent runs in one language, sharing records, options and limits. A task
  starts as a command string and grows into a Lua or Fennel program without changing tools.
- Tasks as discoverable interfaces: one declaration gives strict options, `--help`, completion
  and `timon tasks --json`, for timon's own commands too, so an agent reads the contract
  instead of scraping help text.
- Agent work in the same model as the rest: contracts, run records with lineage, limits by
  scope, cancellation, and explicit outcomes. A review that requests changes, a failed provider
  call and a schema-invalid answer are different results, and none proves the review right.
- Search over past Claude Code and Codex sessions from the same scripts.
- Stock Lua 5.4 and Fennel in one binary. Lua and Fennel need no install; the programs a task
  drives (cargo, git, claude, codex) and their logins still do.

## Gap decisions

After phases 5 and 6, each gap from the first version of this page:

| Gap | Decision |
|---|---|
| Task metadata | Built: aliases, groups, private, confirm, platforms. Confirm fails without a terminal unless `--yes`; nothing waits on a question no one can answer. |
| Passing arguments through | Built. Words reach a command task as words; a command string with shell syntax is refused when tasks load. |
| Shell completion | Built, from the same declarations as help. |
| Skipping up-to-date work | `fs.newer` first (#73). `sources` and `outputs` on a task only with defined invalidation. Three mechanisms stay separate: a dependency runs once per invocation, a file check skips work, and `--resume` reuses agent runs. None of them makes replaying effects safe. |
| Verify, review, best of N | Built: library functions in `flow` (#59) over the runner, not a scheduler. A verification gate can be a deterministic check (`sh`), not only another model's opinion. |
| Cost summaries | A summary in `timon runs` that separates known, unknown (Codex) and cached spend (#84). |
| Interactive picker | Deferred: names, the listing, help and completion come first. |
| Services, sandboxing, remote tasks, packages | Separate, and deferred until a workflow needs one. No sandbox is claimed. The user layer covers local reuse, and `timon promote` (#39) is planned; neither is versioned distribution. |

## Decisions shaped by prior art

| Decision | Taken from |
|---|---|
| One task per invocation; the words after the name are its own | babashka |
| `--jobs` runs a task's independent dependencies, not several named tasks | babashka's `run --parallel` |
| Typed options from one declaration, with help and completion; scripts parse flags with `cli.parse` | babashka.cli's `:spec` and `:exec-fn` |
| timon's commands described like tasks, but listed only with `--all` | bb, just and mise keep their own commands out of the task listing |
| Words after `--` passed as words, never rebuilt into shell text | the #77 review; Task and voga |
| Discovery runs no programs, agents or summaries | the #77 review; mise's declared command effects |

Still open, from the same comparisons:

- **Declared effects** (#83). mise marks each command read, write or destructive. timon could declare
  an effect on every function and command, and let that one declaration drive the refusals
  while tasks load, `--dry-run`, and the hints an MCP server gives a client.
- **Subcommands of a task.** babashka's `:cmd` gives a task subcommands with their own options.
- **Cleanup after a task.** Task's `defer` and mise's `depends_post` run work even when a task
  fails.
- **Three reference workflows** (#85) to compare timon with just, mise and babashka on fixed inputs:
  a `check` with no model calls, a data report over JSON and files, and a bounded review with a
  deterministic gate, an interruption and a resume. Each records setup steps, glue code,
  failures and what a second requirement change costs.

## Sources

- babashka: the [babashka book](https://book.babashka.org/), including its task CLI section,
  and voga's [bb comparison](https://github.com/joshrotenberg/voga/blob/main/docs/bb-comparison.md),
  built from bb-playground on babashka 1.13.226.
- voga: its README and `docs/guide/`.
- just: [attributes](https://just.systems/man/en/attributes.html) and
  [parallelism](https://just.systems/man/en/parallelism.html) in the manual.
- Task: the [guide](https://taskfile.dev/docs/guide/).
- mise: [task configuration](https://mise.jdx.dev/tasks/task-configuration.html),
  [`mise run`](https://mise.jdx.dev/cli/run.html) and [MCP](https://mise.jdx.dev/mcp.html).
- Dagger: [MCP](https://docs.dagger.io/features/mcp).
- Lark: [bmatsuo/lark](https://github.com/bmatsuo/lark).
- Lune: [lune-org/lune](https://github.com/lune-org/lune).
- Flick: [codeberg.org/verdantislet/flick](https://codeberg.org/verdantislet/flick).
- tal: [pt-main/run](https://pkg.go.dev/github.com/pt-main/run).
- Subtask: [zippoxer/subtask](https://pkg.go.dev/github.com/zippoxer/subtask@v0.1.1).
- ai-dispatch: [docs.rs/ai-dispatch](https://docs.rs/ai-dispatch/10.6.0/aid/).
- The review these changes come from: the comment on
  [#77](https://github.com/joshrotenberg/timon/pull/77).
