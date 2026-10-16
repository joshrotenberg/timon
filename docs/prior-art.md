# Prior art

How timon's planned features map to the tools it learns from. Checked 2026-10-16 against
babashka 1.13, voga's main branch, just 1.4x, mise, GNU make, Dagger, Subtask and ai-dispatch
10.6. The design's short version is in [design.md](design.md#prior-art).

In timon's column, **done** means built, **P5** to **P10** is the phase that delivers it (see the
design's Phases), `#N` is the issue, and **not planned** is not on the roadmap. The other columns
say yes, no, or how the tool does it.

## Tasks

| Feature | timon | babashka | voga | just | mise | make |
|---|---|---|---|---|---|---|
| Where tasks live | P5: `tasks.lua`, `scripts/` | `bb.edn` | `voga.toml`, naming `.rhai` files | `justfile` | `mise.toml`, `mise-tasks/` | `Makefile` |
| Language | Lua 5.4, Fennel | Clojure (SCI) | Rhai | just syntax over a shell; shebang recipes in any language | a shell; file tasks in any language | make over a shell |
| List tasks with descriptions | P5 | `bb tasks` | `voga tasks` | `just --list` | `mise tasks` | no |
| Run as `tool NAME` | P5 | yes | yes; built-ins win | yes | `mise run NAME` | yes |
| Dependencies, each run once | P5 | `:depends` | `depends` | yes | `depends`, `depends_post`, `wait_for` | prerequisites |
| A dependency's result goes to the task | P5 | yes | yes | no | no | no |
| Independent tasks in parallel | P6: `--jobs` | `run --parallel` | `--jobs` | `[parallel]` | `jobs` | `-j` |
| Call a task from code | P6: `task.run` | `(run 'task)` | `task::run` | recursive `just` | recursive `mise run` | recursive `$(MAKE)` |
| Typed options and generated `--help` | P6 | babashka.cli specs | yes | parameters, `[arg]` flags | `usage` specs | variables |
| Pass arguments through | not planned | `*command-line-args*` | `passthrough` | variadic parameters | `raw_args` | no |
| Exit status chosen by the task | done (#39) | `:babashka/exit` | `exit` | the command's | the command's | the command's |
| Dry run | P6 (#38) | no | `--dry-run` | `--dry-run` | `--dry-run` | `-n` |
| User and project layers | done for scripts and agents; P5 for tasks | project only | user layer, `promote` | `--global-justfile` | global config | `include` |
| The tool's own commands are tasks | P6 | no | no | no | no | no |
| Aliases | not planned | no | `aliases` | `alias` | `alias` | no |
| Groups in the listing | not planned | no | `group` | `[group]` | name prefixes (`lint:x`) | no |
| Private tasks | not planned | `:private` | `private` | `[private]` | `hide` | no |
| Confirm before running | not planned | no | `confirm` | `[confirm]` | `confirm` | no |
| Platform-specific tasks | not planned | no | `platforms` | `[linux]`, `[macos]` and others | `run_windows` | no |
| Interactive picker | not planned | no | `voga choose` | `--choose` | `mise run` with no task | no |
| Shell completion | not planned | a documented snippet | yes | yes | yes | from the shell |
| Skip work that is up to date | not planned | `fs/modified-since` | `fs::modified_since` | `[cache]`, unstable | `sources`, `outputs` | file times |
| Watch files | P10, to discuss | no | schedules instead | no | `mise watch` | no |
| Load `.env` | P10, to discuss | no | no | `dotenv-load` | `env` | no |
| Remote tasks or packages | not planned | `:deps` | no | no | remote `file`, includes | no |

Flick and tal are left out of the table. Flick's `flick.lua` returns a table whose values are
shell strings or Lua functions, run on LuaJIT; #68 takes its shell-string shorthand. tal runs
`.task.lua` files inside the `run` script manager, with incremental builds keyed on SHA-256.

## Runtime and toolbox

just, mise and make run shell commands, so their toolbox is whatever the shell finds.

| Feature | timon | babashka | voga |
|---|---|---|---|
| REPL | #11 | REPL and nREPL | no |
| Evaluate an expression | done: `eval` | `-e` | `voga eval` |
| Read stdin as data | P7 (#71) | `-i`, `-I`, `-o`, `-O`, `--stream` | no |
| Shebang scripts | done | yes | no |
| Concurrency in the language | done: `scope`, `spawn`, `par` | futures, `pmap`, core.async | `process::start`, `task::run` with `jobs` |
| Run a program, raise on failure | done: `sh` | `shell` | no; results are data |
| Run a program, result as data | done: `proc.run` | `process` | `process::run` |
| Background processes | done: `proc.spawn` | `process` | `process::start` |
| Process groups, timeouts, cleanup | done | `destroy-tree`, by hand | yes |
| Pipelines | #76 | `pipeline` | `process::pipeline` |
| Files and paths | done; more in #73 | `babashka.fs` | `fs` |
| JSON and queries | done: `json`, JMESPath | cheshire, Clojure's functions | `json`, JMESPath |
| TOML, YAML, CSV | P7 (#72) | YAML, CSV, EDN, Transit | TOML, CSV; YAML planned |
| HTTP client | P7 | `babashka.http-client` | `http` |
| git | P7 | through `shell` | `voga/git`, read only |
| GitHub | P7 | through `shell` | `voga/gh` |
| SQLite | P7, bundled | a pod | no |
| Edit files | #51 | `spit` | `edit` |
| Parse a script's arguments | #74 | `babashka.cli` | task options only |

## Runs

| Feature | timon | babashka | voga | mise | Dagger |
|---|---|---|---|---|---|
| A record of each run | done | no | yes, journaled | no | traces |
| Reuse results on a rerun | done: `--resume` | no | no; an interrupted run is marked uncertain | `sources`, `outputs`; a cache, experimental | content-addressed cache |
| Cancellation reaches child processes | done | by hand | yes | not checked | containers |
| Spending budgets | done: runs and USD, nested | no | no | no | no |
| Progress output | done: `--progress`, `prefix` | no | no | `output` modes | a TUI |
| Schedules | P10 (#35) | no | yes | no | no |
| Long-running services | not planned | no | yes, with readiness | `daemons`, experimental | services |
| Sandboxing | not planned | no | project-local writes | `deny_*`, `allow_*` | containers |

## Agents

| Feature | timon | voga | Dagger | Subtask | ai-dispatch |
|---|---|---|---|---|---|
| How agents run | done: the Claude Code and Codex CLIs | an example task over processes | an `LLM` type calling model APIs | a Claude Code skill and CLI | agent CLIs behind one interface |
| Agents supported | Claude Code, Codex | any CLI | model providers | Claude Code, Codex | claude, codex, gemini, copilot, opencode, cursor and others |
| Answers validated against a shape | done: contracts | no | typed environment outputs | not documented | not documented |
| A worktree per run | done: `worktree = true` | no | containers instead | yes, per task | `--worktree` |
| Parallel and background runs | done: `spawn`, `par` | `--jobs` | engine concurrency | yes | `--bg`, `batch --parallel` |
| Dependencies between agent tasks | done in code; P5 for tasks | `depends` | in code | no | `depends_on` in batch files |
| Records and logs | done | run records | traces | progress tracking | JSONL logs, SQLite |
| Cost reporting | done: per run and per script | no | not documented | not documented | `aid usage` |
| Enforced budgets | done: runs and USD, nested | no | no | no | budgets in config |
| Continue with feedback | done: `agent.continue` | no | prompts build on each other | request changes | `aid retry --feedback` |
| Verify, review, best of N | P8 (#59) | no | an agent loop in code | a review step | `--verify`, `--peer-review`, `--best-of` |
| Resume a workflow from its runs | done: `--resume` | no | the cache | not documented | not documented |
| Search past sessions | done: `sessions` | no | no | no | no |
| Teach the host agent | done: a plugin for Claude Code and Codex | built-in skills | no | a Claude Code skill | a prompt for `CLAUDE.md` |
| Memory | P10 | no | no | no | `aid memory` |

## Extension and MCP

| Feature | timon | babashka | voga | Dagger | ai-dispatch |
|---|---|---|---|---|---|
| Call out-of-process tools | P9: MCP client | pods | `mcp::connect` | `LLM.withMCPServer` | no |
| Serve tasks as MCP tools | P9 (#34) | no | `voga mcp` | modules as an MCP server | `aid mcp` |
| Bundled docs for agents, with tested examples | done: skills | no | skills | no | no |

## What only timon combines

- Agent runs and ordinary tasks in one language, with the same records, budgets and options.
- Spending budgets that nest by scope and stop a run before it starts.
- Resume that reuses a workflow's successful agent runs.
- Search over every past Claude Code and Codex session.
- Lua and Fennel on one VM, in one binary with no runtime to install.

## Gaps to decide

Prior art has these and timon does not plan them:

1. **Task metadata**: aliases, groups, private tasks, confirmation, platforms (voga, just,
   mise). Each is a field on the task table, checked when tasks load. Candidates for #68 or
   phase 6.
2. **Passing arguments through** to a wrapped program (voga, mise, just, babashka). A candidate
   for #68: words after `--` go to the task as a list.
3. **Shell completion** from the task declarations (voga, just, mise). A candidate for phase 6.
4. **Skipping work that is up to date** (make, mise, voga, tal). `fs.modified_since` could go in
   #73; `sources` and `outputs` on a task could follow.
5. **Verify, peer review and best of N** as run options (ai-dispatch). These map to #59's
   patterns. `aid usage`'s spend by day and agent suggests a cost summary in `timon runs`.
6. **An interactive picker** (voga, just, mise). Agents and scripts run tasks by name.
7. **Services, sandboxing, remote tasks and packages** (voga, mise, Dagger, babashka). Not
   planned; the user layer and `timon promote` (#39) cover sharing.

## Sources

- babashka: the [babashka book](https://book.babashka.org/) and voga's
  [bb comparison](https://github.com/joshrotenberg/voga/blob/main/docs/bb-comparison.md), built
  from bb-playground on babashka 1.13.226.
- voga: its README and `docs/guide/`.
- just: [attributes](https://just.systems/man/en/attributes.html) and
  [parallelism](https://just.systems/man/en/parallelism.html) in the manual.
- mise: [task configuration](https://mise.jdx.dev/tasks/task-configuration.html) and
  [`mise run`](https://mise.jdx.dev/cli/run.html).
- Dagger: [MCP](https://docs.dagger.io/features/mcp).
- Flick: [codeberg.org/verdantislet/flick](https://codeberg.org/verdantislet/flick).
- tal: [pt-main/run](https://pkg.go.dev/github.com/pt-main/run).
- Subtask: [zippoxer/subtask](https://pkg.go.dev/github.com/zippoxer/subtask@v0.1.1).
- ai-dispatch: [docs.rs/ai-dispatch](https://docs.rs/ai-dispatch/10.6.0/aid/).
