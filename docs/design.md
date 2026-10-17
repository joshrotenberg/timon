# timon design

Status: phases 0 to 9 and host hooks are built. What is not built says Not yet, with its issue.
`README.md` is canonical for what exists; this document is canonical for direction.

timon (Venetian for rudder) is babashka for Lua and Fennel, with native agent tools: a task
runner and scripting toolbox for repository work in one self-contained binary, with coding-agent
and session APIs that are first class but optional. Tasks and scripts are written in Lua or
Fennel, over a toolbox for the work around a codebase: files, JSON, processes, git, HTTP, GitHub
and SQLite. Everything timon does is a task: its own commands are listed and described the way a
project's tasks are, from one declaration each.

It should be worth using with no agent involved, and need no bespoke harness when an agent
becomes one step of a task.

Agents are first class among them. Running Claude and Codex as subagents, reading every past
session and calling any MCP server are modules any task can use, with the same records, budgets
and options as everything else. The skills that teach timon are bundled in the binary, and it
ships as a plugin for Claude Code and Codex, so an agent runs timon's tasks the way a person
does.

## Why this exists

The ideas behind it were tried separately:

| Earlier work | What it showed |
|---|---|
| due | A predictable run contract around `claude -p` and `codex exec` made runs composable. Agents calling it then wrote their own ad hoc harness to loop, retry, fan out and compare. |
| due's Lua composition spike (draft #24) | mlua with vendored Lua 5.4 and Rust owning admission, cancellation and receipts works. Suspended Lua functions made concurrent runs read like sequential code. |
| finocchio | Fennel runs on the same VM for free. The project then drifted into designing a language: immutable data, contracts on functions, a semantic analyzer. |
| babagent | Agents get far more out of data (git, JSON, processes) than out of text, and a language over the toolbox makes pipelines short. It needed babashka installed and shelled out to every other tool. |
| trovador, tosi | History and delegation are useful daily. As separate services they cannot share a script. |
| voga, lesto, bharli, squero | Task runners and Lisps built from scratch. Each spent its time on the runner or the language rather than on the APIs. |

timon takes the parts that worked and drops the parts that did not. voga had the right model:
named tasks with dependencies, typed options and retained runs. Its cost was the runner and the
language. timon keeps the model on stock Lua, with stock Fennel beside it, and adds the agent
and session work that voga and babagent called out to. All the effort goes into the APIs.

### Prior art

Checked 2026-10-16 against the tools below. Each overlaps with timon; none of them, as checked,
puts a scripting-language task runner, its toolbox, and agent and session APIs with shared
records and limits in one binary. That combination is the target, not a claim that the parts
are new:

| Tool | What it is | How timon differs |
|---|---|---|
| babashka tasks | Clojure task runner and toolbox, the model for voga and timon | Lua and Fennel; agents, sessions and contracts built in |
| just, Task, mise | command runners: the bar for how little an ordinary task takes | a programming language and toolbox behind the same ease |
| Lark, Lune, Flick, tal | Lua task runners and a Luau runtime | stock Lua and Fennel, a full toolbox, records, limits and agents |
| Dagger | a container-based workflow engine with Go, Python and TypeScript SDKs and LLMs as a core type | no containers or engine; drives the Claude Code and Codex CLIs with their sessions; one binary |
| Subtask, ai-dispatch | CLIs that dispatch Claude Code and Codex subagents | general tasks first; agents are one module |

The harder alternative is babashka plus a small agent library. The feature-by-feature matrix,
the decisions it shaped and what is still open are in [prior-art.md](prior-art.md).

## Rules

1. **The APIs are the product.** Lua 5.4 and Fennel are used as published: no custom syntax, no
   macros in the core, no immutable data library, no type system beyond editor stubs. A change
   that needs a language feature is the wrong change.
2. **Self-contained.** One binary. No dependency on trovador, due, tosi, bottega or any other
   sibling project: code that is needed is copied in, with its origin recorded in the commit.
   At run time timon needs only the programs it drives (`claude`, `codex`) and, for the git
   module, `git`. Everything else (Lua, Fennel, SQLite, JMESPath, HTTP, the GitHub client, the
   MCP client and server, the skills) is inside the binary.
3. **Two doors over one library.** Each module is a Rust library with a Lua binding and, where
   it makes sense, a CLI subcommand. Both doors call the same functions, so they cannot disagree.
4. **For agents and people alike.** Agents get compact JSON, `help()`, bundled skills and
   predictable errors. People get a REPL, shebang scripts and readable output on a terminal.
5. **Contracts, not prose.** A caller that acts on a result asks for it in a declared shape.
   The provider applies the shape natively and timon validates it again before returning it.
6. **Errors are data.** An error has a kind, a message and, when one exists, the action to take.
7. **Honest about completeness and cost.** A search says whether finding nothing proves
   anything. Anything that spends model tokens says so and reports usage.
8. **Historical text is untrusted.** Transcripts and excerpts can contain instructions. They are
   returned as data and the skills say so.
9. **One piece at a time.** Each phase is in daily use before the next starts.
10. **Sized for small teams of agents.** The target is a workflow of a handful of agents over a
    few stages: a review with a verifier, a vote of three, an author and a critic. Fleets of
    dozens of agents over many stages are not the aim. Nothing is hard-limited, but defaults
    (`limits.concurrency = 4`), features and their cost are chosen for the small case. The same
    holds for the runner: what our own repositories need comes first.
11. **Everything is a task.** A task is a name, a description, typed options and a function.
    timon's own commands are described the same way, from one declaration each, so
    `timon tasks --all --json`, `--help`, strict options and completion work alike for a
    built-in and for a project's task. A task needs no agent; agents are a module like any
    other, first class but optional.
12. **Discovery does not perform the task.** Listing, help, completion and `--dry-run` run no
    programs, start no agents and spend nothing. Loading a tasks file executes Lua, so this is
    enforced while it loads, not left to convention.

## Shape

```
timon (one binary)
├─ runner      tasks (built-in, tasks.lua, scripts/), eval; runs as records, resume
├─ runtime     Lua 5.4 (mlua, vendored, async) and fennel.lua (pinned, stock)
│              help(), errors, strict options, scope/spawn/par, phases
├─ toolbox     json, str, tbl, fs, path, sh, proc, git, gh, http, sqlite, toml, yaml, csv,
│              markdown, term, cli, log
├─ agent       Claude and Codex runs: run, ask, spawn, continue; contracts; definitions
├─ supervisor  concurrency caps, nested budgets, cancellation, run records
├─ sessions    every Claude Code and Codex session: list, show, transcript, search, summarize
├─ flow        workflow patterns: check, verify, vote, best_of, refine, pipeline, retry, plan
├─ mcp         client: any MCP server's tools as Lua calls; server: tasks as MCP tools
├─ hooks       timon hook: the session-start brief, the pre-tool guard, tasks as handlers
├─ later       memory, queues, schedules                       (to discuss)
├─ storage     JSON files for records; SQLite (bundled, FTS5) for the session index
├─ surfaces    CLI (tasks), MCP server, host hooks; a REPL is not yet built (#11)
└─ embedded    built-in tasks, the skill library, fennel.lua
```

A person's own tasks, agents, contracts, skills and scripts live in `~/.config/timon/`, a
project's in `.timon/`, in the same format (see Directories).

## API conventions

These hold for every module, so an agent that has used one module can guess the next.

- **Modules are globals.** `agent`, `sessions`, `json`, `fs` and the rest are global tables in
  every script and in `eval`; there is nothing to `require`.
- **Subject first, options last.** A function with one obvious subject takes it positionally and
  its options as a trailing table: `fs.read(path)`, `sessions.show(id)`. A function with no
  single subject takes one table: `agent.run{ prompt = "...", output = "review" }`. Lua's
  `f{...}` call syntax makes both short.
- **Failures raise; outcomes are data.** Bad arguments, I/O errors and broken invariants raise,
  so a script stops at the first problem the way `set -e` would. Outcomes a script branches on
  are returned as records and never raise: an agent run that failed, an HTTP 404, a process that
  exited 1. Where both are useful there are two functions: `agent.run` returns the record,
  `agent.ask` returns the answer or raises.
- **Errors are tables.** `{ kind = "auth_expired", message = "...", hint = "run claude /login",
  data = {...} }` with a `__tostring`, so tracebacks read well. `try(f, ...)` returns
  `true, value` or `false, err` without losing the table, which `pcall` on a string would.
- **Records are plain tables.** Field names are `snake_case`, timestamps are RFC 3339 strings,
  durations are milliseconds in fields ending `_ms`, sizes are bytes. No userdata in results
  except handles and connections.
- **JSON round-trips.** `json.null` stands for null. Decoded arrays carry a marker so they encode
  as arrays even when empty; `json.array{}` and `json.object{}` make empty ones explicitly. An
  array with holes is an encode error, not a silent object.
- **Options are strict.** Every function that takes an options table declares its keys and their
  types, and checks them before doing anything. An unknown key raises `bad_option` with the
  closest known key, so a typo fails before a paid run starts:

  ```
  timon: bad_option: unknown option "promt" to agent.run
    hint: did you mean "prompt"?
  ```

  Each option's type is declared once, in LuaLS annotation syntax (`"read" | "write" | "full"`,
  `string[]`, `{ usd: number?, turns: integer? }`, `string | schema`), and parsed into the same
  shapes `schema` builds. A wrong type, a value outside an enum (with the closest value as the
  hint) or a bad nested field raises with its path, such as `budget.turns`. Agent definition
  files are checked against the same declarations, and the shapes give each function's options
  as JSON Schema.
- **Globals are strict.** Reading a global that does not exist raises `undefined_global` with
  the closest known name, so `agnet.run` fails on its own line instead of later on a nil. A
  script may define globals only at the top level of its main chunk; `rawget(_G, name)` tests for
  one without raising.
- **Everything is cancellable.** Any call that waits can be cancelled from outside, and a
  cancelled call raises `cancelled`.
- **Documented at run time.** `help(agent)` describes a module and `help(agent.run)` one
  function's signature, options and an example, as text; `help("agent.run")` works too. The
  option declarations behind strict options are the same source, and they also generate the
  `timon` skill's API reference, so the checks, the help and the skills cannot drift apart.
  LuaLS stubs are not built yet.

## Runtime

### Lua version

Lua 5.4, through mlua with the interpreter compiled in. Luau was considered and set aside:

| | Lua 5.4 | Luau |
|---|---|---|
| Fennel | a supported target | not a supported target; no `_ENV` |
| Types | LuaLS annotations and stubs, plus strict options and globals at run time | annotations parse, but mlua's Luau build (`luau0-src`) ships no type checker, so nothing checks them unless timon builds Luau's analysis library in |
| Numbers | 64-bit integers and floats | floats only; integers above 2^53 lose precision |
| Cleanup | `<close>` | none; `scope(fn)` only |
| Sandbox | custom environments and hooks | read-only globals, safe environments, interrupts |
| Evidence | validated by the due and finocchio spikes | not tried |

The only reason to revisit is real type checking compiled into the binary (`timon check`), and
only if Fennel keeps working. Without the checker, Luau's types are not worth the move. mlua
selects the Lua version with a cargo feature, and nothing essential in timon depends on a
5.4-only feature, so the door stays open.

### Evaluating code

```sh
timon eval 'agent.ask("Name three Lua web frameworks")'
timon eval -f review.lua
echo 'sessions.list{ limit = 3 }' | timon eval -
timon eval --fnl '(sessions.search {:query "mlua"})'
```

`eval` treats a chunk that parses as an expression as `return <chunk>`, as the Lua REPL does,
and prints the result. Strings print raw; tables print as JSON, indented on a terminal and
compact otherwise. Logs and progress go to stderr, so stdout stays parseable.

### Scripts

```lua
#!/usr/bin/env timon
-- Review the given pull requests: timon review.lua 12 14 15
local prs = tbl.map(arg, tonumber)
...
```

`timon FILE.lua [ARGS]` and `timon FILE.fnl [ARGS]` run a file with the standard `arg` table and
`...`. `timon NAME [ARGS]` runs a named script from a `scripts/` directory, which is a task (see
Tasks).
A script's last returned value is printed the way `eval` prints.

### REPL

Not yet (#11). `timon repl` (and `timon repl --fnl`) with history, completion of module and field names from
the same metadata as `help()`, and printing as above.

### Concurrency

Calls that wait (an agent run, a search, a tool call) are async Rust functions. A script runs as
a Lua coroutine on a tokio runtime, so those calls read as ordinary sequential code and yield
underneath.

Concurrency is structured, and `scope` is its primary form. Work runs inside a scope, the scope
does not end until every child has finished, and a failure cancels the siblings:

```lua
local review, history = scope(function(s)
  local r = s:spawn(agent.run, { agent = "reviewer", prompt = "Review PR 12" })
  local h = s:spawn(sessions.search, { query = "PR 12", since = "30d" })
  return r:await(), h:await()
end)
```

- `scope(fn)` calls `fn(s)` and returns what `fn` returns, after every child spawned on `s` has
  finished. Nothing outlives its scope, so nothing leaks.
- If `fn` or any child raises, the scope cancels the remaining children, waits for them to
  stop, and raises the first error. A cancelled agent run is recorded as `cancelled`.
- `s:spawn(f, ...)` starts `f(...)` as a child and returns a handle: `h:await()` returns its
  values or raises its error, `h:cancel()` is idempotent, and `h:done()` does not wait.
- `s:cancel()` cancels every child still running, for example once one result is enough.
- A script's main chunk is a scope, and the global `spawn(f, ...)` spawns on the innermost
  active scope. A script that spawns without awaiting still waits for its children before it
  exits, and Ctrl-C cancels them all.

The `par` helpers are scopes underneath:

```lua
local out = par.map(items, fn, { limit = 4 })                  -- input order; an error cancels the rest
local out = par.map(items, fn, { limit = 4, settle = true })   -- { ok, value | error } per item
local a, b = par.all(f1, f2)                                   -- every result; an error cancels the rest
local first, i = par.race(f1, f2)                              -- the first to finish; the rest are cancelled
local i = par.first(tasks)                                     -- which task finished first; cancels nothing
sleep("2s")
```

- `phase(name)` names what the current task is doing. Tasks inherit the phase of the task that
  spawned them, and `s:spawn` takes the phase of the scope's function, so siblings never
  overwrite each other's. Agent runs record their task's phase, and `--progress text|json`
  prints a line on stderr when each run starts and ends (on by default only on a terminal).
- Each task is identified by its own coroutine, which Lua passes in (`coroutine.running()`):
  inside a callback, mlua's `current_thread` does not name the calling coroutine.

`local s <close> = scope()` is Lua 5.4 shorthand for the function form: when `s` goes out of
scope it waits for its children, or cancels them if the block is leaving with an error. It is a
convenience only. Nothing in timon requires it, so code written with `scope(fn)` runs unchanged
if the runtime ever moves off 5.4 (see Lua version). Fennel uses the function form,
`(scope (fn [s] ...))`.

One Lua state runs per invocation on one thread. Long CPU work in Lua blocks other coroutines,
while waiting does not.

### Fennel

The pinned `fennel.lua` loads on first use. `.fnl` files work for scripts, `tasks.fnl`, `lib/`,
agents and contracts, and `eval --fnl` takes Fennel; `init.lua` and `mcp/` take Lua only. Fennel code calls the same
globals:

```fennel
(let [r (agent.run {:prompt "Review PR 12" :output :review})]
  (when (= r.answer.verdict :request_changes)
    (print (json.encode r.answer.findings))))
```

The skills show Lua first, because agents read and write it most reliably, with Fennel
equivalents where they help.

### Core modules

| Module | Functions |
|---|---|
| `json` | `encode(v, {pretty})`, `decode(s)`, `query(v, expr)` (JMESPath with jpx's extension functions), `null`, `array(t)`, `object(t)` |
| `str` | `split`, `trim`, `lines`, `starts`, `ends`, `fmt` (`"PR ${n}"` templates); also installed as string methods, so `s:trim()` works |
| `tbl` | `map`, `filter`, `reduce`, `find`, `keys`, `values`, `group_by`, `sort_by`, `count`, `merge`, `pick`, `omit` |
| `fs` | `read`, `write`, `exists`, `ls`, `glob`, `mkdir`, `tmpdir`, `cwd`, `home`; `path.join`, `path.base`, `path.dir`, `path.ext` |
| globals | `help`, `try`, `scope`, `spawn`, `phase`, `par`, `sleep`, `pp` (pretty-print to stderr), `log.info/warn/error` |

The stock Lua standard library stays intact.

## Tasks

Built in phases 5 (#68) and 6 (#79, #80).

```lua
-- .timon/tasks.lua (and ~/.config/timon/tasks.lua, or tasks.fnl)
return {
  test = "cargo test",                                  -- a command for sh
  check = {
    description = "The gates: format, lint, test, docs",
    depends = { "test" },
    run = function(opts, deps) return "all gates pass" end,
  },
  review = {
    description = "Review the diff against a base",
    depends = { "check" },
    budget = { runs = 3, usd = 2 },
    aliases = { "r" },
    options = { base = { type = "string", default = "main", short = "b", doc = "the branch" } },
    run = function(opts, deps)
      return agent.ask{ agent = "reviewer", prompt = "Review the diff against " .. opts.base }
    end,
  },
}
```

```sh
timon tasks                          # tasks.lua and scripts/, both layers; --all, --json
timon check                          # a task, when no timon command has the name
timon review -b develop              # opts.base is "develop"; --help prints the options
timon -j 2 review                    # independent dependencies at the same time
timon --dry-run review               # the plan as JSON
timon test -- --nocapture            # words after -- are taken as they are
timon task runs                      # always the task, even when a command has the name
timon --yes drop-db                  # answers confirm
```

- **A task** is a table (`description`, `run`, `options`, `depends`, `budget`, `aliases`,
  `group`, `private`, `confirm`, `platforms`), a command string, or a function. The fields are
  declared like options and checked when tasks load, with did-you-mean. So are unknown
  dependencies, cycles, and aliases that clash with a task, another alias or a command.
- **`run(opts, deps)`.** `opts` holds the typed options and `opts.args` the other words;
  `deps.NAME` is each dependency's first result. A command string runs without a shell and gets
  the words appended as words, so shell syntax in it is refused when tasks load.
- **Options** are declared in the #29 type syntax, alone or as `{ type, default, short, doc }`,
  and parsed by one engine (`src/flags.rs`) that also serves `cli.parse` in scripts. Flags
  follow babashka.cli: `--key value`, `--key=value`, `--flag`, `--no-flag`, a repeated flag for
  a list, a short name, `--`. `timon NAME --help` is generated from the declaration, and
  `timon tasks --json` includes it. Scripts keep `arg` and parse it with `cli.parse` when they
  want flags, as bb scripts use babashka.cli.
- **Scripts are tasks.** Every script in `scripts/` is a task whose description is its leading
  comment, and on its own it runs as a script always has. The other fields are declared in
  `tasks.lua`, because listing reads scripts without running them; long task bodies go in
  `lib/`. A name in both `tasks.lua` and `scripts/` of one layer is an error.
- **Loading.** `tasks.lua` runs with every module, because its `run` functions use them. Its
  top level runs whenever tasks are listed, so programs and agent runs raise there.
- **Layers:** project tasks replace user tasks by name, like skills; a project's tasks load
  only after `timon trust`. timon's commands take precedence over both, and `timon tasks`
  marks a task one of them hides.
- **Plans and records:** dependencies run once each, depth first in declared order, then the
  task; the first failure stops the plan, and an error with `exit` sets the exit status (#39).
  A plan is one script run (#32) named after the task, with the plan in its request and each
  task's status and time in its result, so it is resumable (#57). A task's `budget` scopes the
  agent runs inside it (#56). `confirm` asks before anything runs, and `--yes` answers it;
  `platforms` stops a plan with an unavailable task before anything runs.
- **Plans in parallel.** One task per invocation, as in bb: the words after the name are its
  own. `--jobs N` runs dependencies that do not depend on each other at the same time, level by
  level, with each command task's output prefixed by its name. Several tasks at once are a task
  that depends on them.
- **`task.run(name, opts)`** runs a task from inside `run`, in the same script run: its
  dependencies that have not run in this invocation, then the task, with `opts` checked like
  flags. It does not ask `confirm`.
- **`--dry-run`** prints the plan as JSON: each task, its level, what it runs and its options.
  It cannot see `task.run` calls inside a function.
- **timon's commands as tasks.** `timon tasks --all` lists timon's own commands after the
  tasks, and `timon tasks --all --json` describes each command and subcommand the way it
  describes a task: name, description, options with types in the #29 syntax, defaults. The
  declarations are clap's, read through its command model, so help, parsing and the listing
  come from one place. The default listing shows only tasks, as `bb tasks`, `just --list` and
  `mise tasks` do.
- **Completion** for bash, zsh and fish (`timon completions SHELL`) calls a hidden
  `timon __complete`, which answers from the same declarations and the task catalog: command,
  subcommand, task and alias names, flags, and the choices of an option. Loading for completion
  follows the listing's rule, and a tasks file that fails to load leaves only the commands.

## agent

Runs Claude Code (`claude -p`) and Codex (`codex exec`) as subagents, through their CLIs, so
subscription billing, tools, sessions and authentication all work as they do for a person.
One portable vocabulary covers both providers.

### run

```lua
local r = agent.run{
  prompt   = "Review PR 12 against issue 9",
  agent    = "reviewer",         -- a definition; explicit fields below override it
  provider = "claude",           -- "claude" | "codex"; default from init.lua
  model    = "opus",
  effort   = "high",
  access   = "read",             -- "read" | "write" | "full"
  cwd      = ".",
  worktree = false,              -- true: a fresh git worktree, removed afterwards if clean
  output   = "review",           -- a contract name or a schema value
  system   = "Appended to the system prompt",
  timeout  = "10m",
  budget   = { usd = 2, turns = 30 },
  resume   = earlier.session,    -- continue a session timon started
  fork     = "45d4b2f5",         -- branch from any session; the original is untouched
  events   = false,              -- true: r.events lists text, reasoning, tool calls and results
  native   = { "--add-dir", "../shared" },   -- raw provider arguments, the escape hatch
}
```

`access` is the portable permission level:

| access | Claude | Codex |
|---|---|---|
| `read` | Read, Grep and Glob only | `--sandbox read-only` |
| `write` | `acceptEdits` with edit tools | `--sandbox workspace-write` |
| `full` | `bypassPermissions` | `--sandbox danger-full-access` |

The result is a run record. It never raises for an outcome:

```lua
{
  id       = "r_01JA...",        -- timon's run id
  status   = "ok",               -- "ok" | "failed" | "invalid" | "timeout" | "cancelled"
  answer   = { ... },            -- the validated structured output, when `output` was given
  text     = "...",              -- the final assistant message
  error    = nil,                -- { kind, message, hint } when status is not "ok"
  provider = "claude", model = "opus",
  session  = "8c1f...",          -- the provider's session id; pass to `resume`
  usage    = { input_tokens = 0, output_tokens = 0, usd = 0.42 },   -- usd is nil for Codex
  ms       = 81234,
  events   = nil,                -- with events = true, see below
}
```

With `events = true` the record lists what the agent did, in one shape for both providers:
`{ kind = "text" | "reasoning", text }`, `{ kind = "tool_call", id, name, input }` and
`{ kind = "tool_result", id, error, text }`. Claude streams them (`stream-json`); Codex's come
from its `exec --json` items, with shell commands as `shell` calls. A failed run keeps the events
it produced. Strings are capped at 2,000 characters and a run at 500 events, and the run's
`result.json` stores them redacted.

`invalid` means the provider answered but the answer failed the contract. `error.data` then holds
the validation errors and `text` holds what came back. Execution status and the answer's
verdict are separate: a review that requests changes is `status = "ok"`.

### ask, spawn, continue

```lua
local verdict = agent.ask{ prompt = "Is this safe?", output = "yes_no" }   -- answer, or raises
local text = agent.ask("Summarize docs/design.md in five bullets")        -- text, or raises
local h = agent.spawn{ prompt = "...", provider = "codex" }               -- spawn(agent.run, spec)
local r2 = agent.continue(r, "Now fix the findings you listed")           -- resume r.session, same request
```

`agent.ask` raises an `agent_failed` error whose `data` is the run record.

### Fan-out and review loops

```lua
local prs = { 12, 14, 15 }
local reviews = par.map(prs, function(n)
  return agent.run{ agent = "reviewer", prompt = "Review PR " .. n, output = "review" }
end, { limit = 3 })

for i, r in ipairs(reviews) do
  if r.status == "ok" and r.answer.verdict == "request_changes" then
    local fix = agent.run{
      provider = "codex", access = "write", worktree = true, output = "implementation",
      prompt = str.fmt("Address these findings on PR ${n}:\n${f}",
                       { n = prs[i], f = json.encode(r.answer.findings) }),
    }
    pp(fix.answer)
  end
end
```

### Workflow patterns

`flow` (#59, built) is Lua over the runner: Rust keeps processes, cancellation, capacity,
budgets and records; the patterns keep sequence, rounds, quorum and verification. No scheduler
or graph engine (#60).

- **A pattern returns a record and never raises for an outcome.** `status` says whether it ran
  to the end (`ok`, `failed`, `timeout`, `invalid`); what it concluded is a separate field
  (`winner`, `accepted`, `kept`). Every record has `pattern`, `runs` (the agent records its
  steps returned), `usage { runs, usd, unknown_cost }` and `ms`. Bad arguments raise
  `bad_option`; cancellation still cancels.
- **Gates.** `flow.check(gate, value)` is the one verification primitive: a Lua predicate, a
  command (passes on exit 0) or an agent spec (`yes_no` by default). A verdict keeps whether the
  check ran (`status`) apart from what it concluded (`passed`), so a review that requests
  changes, a failed run and an invalid answer stay apart. `refine` and `verify` take gates.
- **Patterns:** `pipeline` (no barrier between items), `vote` (stops at quorum), `best_of` (a
  deterministic `score` or a `judge`), `refine` (revise until a gate passes), `verify` (keep by
  majority, all or any), `retry` (the last outcome passed on, for repair) and `plan` (steps by
  `needs`; the first failure stops new steps). debate, team, map_reduce, route and tournament
  are compositions of these (#99).

### Contracts

A contract is a named schema. Every object field is required unless it is marked optional, so a
worker cannot quietly leave out "did you run the tests".

```lua
-- contracts/review.lua
return schema.object{
  verdict  = schema.enum{ "approve", "request_changes" },
  summary  = schema.string:describe("Two sentences at most"),
  findings = schema.array(schema.object{
    file     = schema.string,
    line     = schema.int,
    severity = schema.enum{ "high", "medium", "low" },
    detail   = schema.string,
  }),
  tests_run = schema.bool,
  notes     = schema.optional(schema.string),
}
```

`schema` has `object`, `array`, `map`, `string`, `int`, `number`, `bool`, `enum`, `optional`,
`any` and `:describe`, and `schema.contract(name)` loads a named contract. `schema.json(s)` gives JSON Schema 2020-12, which is what both providers
take natively; `schema.check(s, value)` returns `true` or `false, errors`. Built-in contracts
cover common work: `review`, `implementation`, `investigation`, `yes_no`, `list`.

### Supervisor

The supervisor is Rust and owns every child process. Scripts cannot bypass it.

- **Admission.** At most `limits.concurrency` agent runs at once per invocation (default 4);
  further runs queue in order.
- **Budgets.** `--max-runs` and `--max-usd` per invocation, `scope(fn, { budget = { runs,
  usd } })` per part of a script, and `budget` per run. A run counts against every budget it
  is inside, and one that would exceed any of them raises `budget_exceeded` before it starts.
  A Claude run's `--max-budget-usd` is capped at the least any enclosing budget has left.
  `reserve = true` takes a scope's runs from the enclosing budget up front and gives back the
  unused ones when it ends. Codex reports tokens only, so a USD budget counts Claude runs and
  says so.
- **Script runs and resume.** `timon NAME`, `timon FILE` and task plans are script runs, recorded
  with their source's sha256 and totals; their agent runs record `parent`. Each agent run in
  a script gets a key: the sha256 of what decides its answer, and how many identical requests
  came before it in the script run. `--resume ID` reruns the script, and a run whose key
  matches a reusable run of `ID` returns that record (`cached = true`). Reusable means `ok`
  and read-only, `idempotent = true`, or a write in a kept worktree still at the head and
  state the record saw.
- **Orphans.** Each run's record names its provider's pid and process group. If timon dies,
  the next timon command settles the run as `interrupted` and stops the provider, after
  checking that the pid still runs claude or codex and leads its group. On Linux the kernel
  stops it at once (`die_with_parent`).
- **Cancellation.** Ctrl-C, a cancelled handle or an ended script stops the run's process group
  with a grace period, then kills it. Every outcome is recorded, including `cancelled`.
- **Run records.** Each run writes `request.json` and a `running` result under
  `state/runs/<id>/` before the provider starts, then its final result; `state/runs/index.jsonl`
  gets a line for each state. A run whose timon process died is found later and settled as
  `interrupted`, with `effects_uncertain` unless it was read-only; nothing is replayed. `timon runs`, `timon runs show ID`,
  `agent.runs{ since = "1d" }`. Secrets are redacted before anything is written.
- **Worktrees.** `worktree = true` creates a worktree in the state directory on a
  `timon/<run id>` branch, runs there, and removes the worktree if it is clean. A dirty one is
  kept and its path is in the record.
- **Continuation.** `resume` accepts only sessions timon started. `fork` works on any session and
  leaves the original untouched, so a person's own sessions are never continued by a script.

### CLI door

`timon agent run` takes the same fields as flags and prints the record:

```sh
timon agent run --provider codex --output review --access read "Review the diff on this branch"
timon runs --since 1d
```

## sessions

Reads every Claude Code and Codex session on the machine, the history trovador serves today,
reimplemented in timon. It only reads: it never writes to the CLIs' files.

```lua
sessions.list{ backend = "claude", dir = ".", since = "7d", limit = 20 }
-- { sessions = { { id, backend, title, snippet, project, cwd, modified, size_bytes, origin,
--                 resumable, pull_requests }, ... }, hidden = { subagents, automated } }

sessions.show(id)          -- metadata plus the command a person would use to resume it
sessions.current()         -- the session timon is running inside, when the host says so

sessions.transcript(id, { offset = 0, limit = 50, kinds = { "user", "assistant" } })
-- { items = { { index, kind, text, name, count }, ... }, next_offset = 50, has_more = true }

sessions.search{
  query   = "mlua async",
  mode    = "terms",         -- "terms" | "phrase" | "fts" | "regex"
  backend = nil, project = nil, since = "30d", limit = 20,
}                          -- the current session is left out unless exclude = false
-- { hits = { { id, backend, title, ..., matches = { { index, kind, excerpt } } } },
--   exhaustive = true, absence_is_proof = nil, note = "..." }

sessions.summarize(id, { model = "haiku" })
-- { text, model, usd, cached, chunks, chunks_summarized }
```

- Ids accept an unambiguous prefix.
- Search is backed by a SQLite FTS5 index in the cache directory. Every call refreshes it
  incrementally from file sizes and modification times, so it is never stale and never needs a
  daemon. `timon sessions index --rebuild` starts over.
- `exhaustive = false` means finding nothing proves nothing, and `note` says what was not
  covered.
- A record is indexed up to 4,000 characters (600 for tool calls and results). When
  nothing matches and a session in scope has a cut record, `absence_is_proof` is false and
  `coverage.truncated` counts the sessions and records.
- Transcript text is redacted (`[redacted:<kind>]`) and untrusted.
- Summaries run through `agent.run` (`ephemeral`, no tools, one turn), so budgets and run
  records apply. The transcript is redacted before it is sent; a long one is summarized in
  parts and merged. The cache is keyed by the session file's size and modification time.
- The CLI door mirrors the module, plain text by default and JSON with `--json`:
  `timon sessions search mlua --since 30d`.

## mcp

timon is an MCP client and an MCP server, both through tower-mcp.

### Client

Built (#100), on tower-mcp 0.24's stdio client. Any MCP server's tools become Lua calls:

```lua
local crates = mcp.connect("crates")             -- mcp/crates.lua in a layer
local files = mcp.connect{ command = { "npx", "-y", "@modelcontextprotocol/server-filesystem", "." } }

crates:tools()                                   -- { { name, description, input_schema } }
crates:call("get_crate_info", { name = "mlua" }) -- the structured content, or the text
crates:resources()                               -- { { uri, name, mime_type } }
crates:read("crates://mlua/readme")
crates:close()                                   -- also when the opening scope ends
```

- `call` raises `mcp_tool_failed` when the tool reports an error, with the server's text in the
  message and its content in `data`; protocol failures raise `mcp_failed`.
- The server runs in its own process group. A connection belongs to the scope that opened it,
  like `proc.spawn`: when that scope ends, the server's group stops. Starting and initializing
  time out (default 30s).
- `mcp/NAME.lua` is a definition file, `{ description, command, env, cwd }`, loaded without
  effects; `mcp.servers()` lists them, a project's hiding a user's of the same name.
- `mcp.connect` is refused while tasks load (rule 12).
- The CLI door: `timon mcp servers`, `timon mcp tools crates`,
  `timon mcp call crates get_crate_info name=mlua`.
- Not yet: HTTP servers (`url`), whose client brings its own TLS stack.

### Server

Built (#34). `timon serve` runs an MCP server over stdio, through tower-mcp, for hosts that
prefer tools to a shell, or have no shell:

| Tool | Does |
|---|---|
| each public task | runs the task after its dependencies; its input schema is the task's typed options, plus `args` |
| `eval` | evaluates Lua or Fennel and returns the result |

- A result is text plus `{ value }` as structured content; a failure is an error result with
  `{ error = { kind, message, hint, data } }`. A task call is a script run record, as from the
  CLI.
- A task whose plan asks `confirm` takes a required `confirmed`; a call without it is an error
  telling the agent to ask the user. Only the user's answer should set it.
- The skills are resources, `timon://skills/NAME`, so a client without plugin support reads the
  same guidance.
- Each call runs in a fresh runtime on the `LocalSet`, behind a channel from tower-mcp's `Send`
  handlers: calls are isolated, can overlap, and see the current tasks and settings. Cancelling
  a request drops its run, which stops its processes.
- stdout carries the protocol: the transport writes to a copy of fd 1, and fd 1 points at
  stderr, so what tasks print cannot corrupt the stream.
- The tool list is built at start; a changed `tasks.lua` needs a restart (no
  `tools/list_changed` yet). Tool annotations wait for declared effects (#83).

## Directories

Two layers share one format, over what is built into the binary:

| Layer | Where | Holds |
|---|---|---|
| built in | the binary | the skills, contracts and agents that ship with timon |
| user | `~/.config/timon/` (`$XDG_CONFIG_HOME/timon`) | a person's own definitions, available everywhere |
| project | `.timon/` in the nearest ancestor directory that has one | the same, for one project, usually committed |

```
~/.config/timon/  and  <project>/.timon/
├── init.lua              settings for this layer: defaults, limits, hooks, guard (optional)
├── tasks.lua             tasks (or tasks.fnl): `timon NAME`
├── agents/
│   └── reviewer.lua      one agent per file; the file name is the name
├── contracts/
│   └── review.lua        one schema per file
├── skills/
│   └── release/
│       ├── SKILL.md      what Claude Code and Codex read
│       └── references/   optional: sections `timon skill show release --ref NAME` prints
├── scripts/
│   └── triage.lua        a task too: `timon triage [ARGS]`
├── lib/
│   └── util.lua          on package.path for every layer's scripts: require "util"
└── mcp/
    └── crates.lua        an MCP server timon can connect to by name
```

Directory names are plural nouns, file names are definition names, and `.fnl` works everywhere
`.lua` does. Every part is optional: an empty directory is a valid layer.

### Resolution

- A name resolves project first, then user, then built in. A project `reviewer` replaces the user
  one; `extends = "reviewer"` in the file merges onto the next layer down instead.
- `lib/` directories are searched in the same order, before Lua's own path. Lua modules there
  load as ordinary Lua, so third-party libraries work unchanged; Fennel modules load under strict
  globals, like scripts.
- The project layer is the nearest ancestor of the working directory with a `.timon/`
  directory, stopping at the home directory.
- `timon defs` shows the layers, settings, agents, scripts and `lib/` directories in effect;
  `timon skill list` and `timon mcp servers` list skills and MCP servers with their layers.

### Files

```lua
-- init.lua
return {
  defaults = { provider = "claude", model = "opus", effort = "medium", access = "read" },
  limits   = { concurrency = 4, max_usd = 10 },
  hooks    = { SessionStart = { "timon:brief", "status" } },
  guard    = { writes = { "." }, commands = { "git push --force" } },
}
```

```lua
-- agents/reviewer.lua
return {
  provider = "claude", model = "opus", effort = "high", access = "read",
  system = "You review one pull request against its linked issue. Report findings only.",
  output = "review",
  budget = { usd = 2 }, timeout = "10m",
}
```

```lua
-- mcp/crates.lua
return { description = "crates.io", command = { "cratesio-mcp" } }
```

```lua
-- scripts/triage.lua
-- Label every unlabeled issue in this repository.
return {
  description = "Label every unlabeled issue",
  run = function()
    ...
  end,
}
```

- **Definitions load with no effects.** `init.lua` and the files in `agents/`, `contracts/`
  and `mcp/` run in an environment with the pure standard library, `str`, `tbl`, `json` and
  `schema`, and no I/O. Each must return a table, which is validated; an error names the file and
  the field. Settings in `init.lua` merge key by key, project over user over built in.
- **Scripts are programs.** A script runs with every module available. Its description for
  `timon tasks` and `timon scripts` is its leading `--` comment, read without running it. A script may return
  `{ description, phases, budget, run }`. Typed `params` are not built yet, so a script reads
  its words from `arg` (or parses them with `cli.parse`).
- **Trust.** Project files load only after `timon trust` records the directory (in
  `state/trusted`, one project root per line; `timon trust --revoke` removes it). The user layer
  is trusted. Code a person runs directly (`timon eval`, `timon ./file.lua`) needs no trust:
  running it is the person's own act.

## Storage

Definitions are files a person edits. Records timon writes once and reads back whole (runs,
summaries) are JSON files; what needs queries (the session index) is SQLite, bundled through
rusqlite with FTS5 for text search.

```
~/.local/state/timon/        $XDG_STATE_HOME/timon
├── trusted                  trusted project roots, one per line
├── runs/index.jsonl         one line per agent run
├── runs/<id>/               request.json, result.json per agent run
├── scripts/index.jsonl      one line per script run
├── scripts/<id>/            request.json, result.json, runs.jsonl per script run
└── worktrees/<id>/          worktrees for `worktree = true` runs
~/.cache/timon/              $XDG_CACHE_HOME/timon
├── index.db                 the session index; safe to delete, rebuilt on the next search
└── summaries/               one JSON file per summarized session
```

- XDG locations are used on macOS as well, as most command-line tools do, so config, state and
  cache are in the same places on every machine.
- `TIMON_HOME` puts config, state and cache under one root. Tests set it to a temporary
  directory and assert that no path escapes it.
- Directories are 0700 and files 0600, because they hold transcript text.
- timon writes nothing into a repository except what a person puts in `.timon/` and worktrees
  it is asked for.
- The session index carries a schema version; a different one is rebuilt, since it is a cache.
- Removing the state and cache directories removes everything timon wrote.

## Bundled skills

The skill library is embedded in the binary:

```sh
timon skill list
timon skill show timon            # prints SKILL.md
timon skill show timon --ref api   # the generated API reference
```

| Skill | Teaches |
|---|---|
| `timon` | What timon is, `eval` and scripts, the conventions above, the module index, errors, untrusted text, cost |
| `timon-agents` | `agent.run`, contracts, fan-out, author and review loops, worktrees, budgets |
| `timon-sessions` | Finding earlier work before answering from memory |
| `timon-scripting` | Scripts, directories, layers, trust, Fennel |
| `timon-tasks` | `tasks.lua`, options, dependencies, `--dry-run`, tasks over MCP and as hooks |
| `timon-workflows` | `flow`: gates, verify, vote, best of N, refine, pipelines, retry, plans |

Each skill is short, because context is the scarce resource; the `timon` skill's API reference
is generated from the declarations and printed on demand. A skill joins the library with the module it teaches.
The plugin ships the same files, generated from the binary, so a host with plugin support and
one without read identical guidance.

## Plugin

The repository is its own marketplace:

```sh
claude plugin marketplace add joshrotenberg/timon
claude plugin install timon@timon
```

- `plugin/` holds both hosts' manifests (`.claude-plugin/plugin.json` for Claude Code,
  `.codex-plugin/plugin.json` for Codex), the hooks, the skills and a `bin/timon` shim. Neither manifest registers
  `timon serve` yet; `claude mcp add timon -- timon serve` adds it by hand.
- `timon plugin build plugin/` writes `skills/` from the skills in the binary; the manifests,
  `hooks/hooks.json` and the shim are checked-in files. `timon plugin build --check plugin/`
  fails when `skills/` differs, and CI runs it.
- The shim runs `TIMON_BIN`, then `$CARGO_HOME/bin/timon`, then `timon` on PATH, and otherwise
  prints the `cargo install --locked --git` command. For `hook` it always exits 0 (#107).
  Prebuilt binaries are an open question.
- A rule carried over from bottega: no `version` fields.
- `hooks/hooks.json` is one file for both hosts: both load it from a plugin, accept the same
  event names, matchers and JSON output, and substitute `${CLAUDE_PLUGIN_ROOT}` in the command.
  See Hooks.
- Codex's manifest is `.codex-plugin/plugin.json`, not the portable root `plugin.json`. Codex
  0.162 reads a root `plugin.json` first and then loads no plugin hooks, with or without a
  `hooks` entry (#104); from `.codex-plugin/plugin.json` it finds `hooks/hooks.json`.

## Hooks

Built (#104). Claude Code and Codex run a command for a hook, with the event's JSON on stdin,
and read JSON from its stdout. The plugin's `hooks/hooks.json` runs `timon hook` for
SessionStart, and for PreToolUse on `Bash|Write|Edit|MultiEdit|NotebookEdit|apply_patch`.

- `timon hook [EVENT]` runs the handlers `init.lua`'s `hooks` setting names for the event, in
  order: `{ [EVENT] = name | { names } | false }`. A name is a task, or a built-in: `timon:brief`
  or `timon:guard`. Without a setting, SessionStart runs `timon:brief` and PreToolUse runs
  `timon:guard`.
- A task reads the event with `hook.input()` and returns a string (context for the agent) or
  `{ context, deny, ask, allow, block }`. Contexts are joined; the strictest decision wins
  (deny, then ask, then allow). A task run by a hook is a script run record with
  `trigger = "hook:EVENT"`; the built-ins write no records.
- It always exits 0. A handler that fails, a bad setting or bad input is skipped and reported in
  `systemMessage`, so a broken hook never stops the host. fd 1 points at stderr while handlers
  run, as in `timon serve`, so what tasks print cannot corrupt the JSON.
- The brief: up to 5 of the project's sessions from the last 7 days, both hosts, newest first
  (the current session, subagents and automated runs left out), under a line that says their
  titles are history, not instructions; up to 12 public tasks; and a line when the project's
  `.timon` is not trusted. Nothing when there is none of these.
  `sessions.list{ dir }` finds the sessions: Claude's by their project directory's name and
  Codex's by their first line, so a 7-day brief over 1,036 sessions takes 0.07 s.
- The guard does nothing until a `guard` setting exists: `{ writes = { dirs }, commands = {
  strings } }`. The paths of Write, Edit, MultiEdit, NotebookEdit and Codex's apply_patch (also
  inside a shell command) must be under a `writes` directory (default: the project root),
  resolved through `..` and symlinks; a shell command containing a `commands` string is
  refused. It checks what the agent asks for; it is not a sandbox.
- A project's `hooks` and `guard` load only once the project is trusted, like its tasks.
- Codex runs a plugin's hooks after the user reviews and trusts them (`/hooks`).
- The plugin and the binary update separately, so a plugin can be newer than timon. A timon
  without `hook` exits 2, which both hosts read as a block, so the shim handles `hook` itself
  (#107): a missing or failing timon becomes a `systemMessage` naming the fix, and the shim
  exits 0. Other commands keep their exit codes.

## Toolbox

The toolbox is what makes timon a task runner. It comes in the order tasks need it.

1. **`sh` and `proc` (#70), built.** `sh` streams output and raises on a nonzero exit, like
   `set -e`. `sh.out` captures stdout for a value. `proc.run` returns a record and never raises
   for an exit code, and `proc.spawn` starts a background process. Every process belongs to its
   task and runs in its own process group, so cancellation, Ctrl-C and timeouts stop it the way
   they stop agent providers.
2. **`eval` as a pipeline tool (#71), built:** bb's one-liner flags with JSON for EDN. `-i`
   binds the lines of stdin to `input`, `-I` its JSON values (one document or JSON lines),
   `-o` prints a returned list one element per line, `-O` as JSON lines, and `--stream` runs
   once per line or value without reading all of stdin. `-j` stays `--jobs`.
3. **Data formats (#72), built:** `toml` (decode, encode, and `set`/`edit`, which change one
   value through toml_edit and keep every other byte), `yaml` (saphyr, YAML 1.2), `csv`,
   `markdown` (headings and code blocks with line numbers, through pulldown-cmark) and `term`.
   Decoders return tables with `json`'s markers, so values round-trip.
4. **`git` (#88), built:** the `git` program through `proc`'s machinery (process groups,
   cancellation, `GIT_TERMINAL_PROMPT=0`, refused while tasks load), with porcelain output
   parsed into data. Worktrees are `git.worktree_add`, `git.worktrees` and
   `git.worktree_remove`; `git.run` covers the rest. A failure raises `git_failed` with git's
   stderr.
5. **`http` and `gh` (#90), built:** `http` on reqwest with rustls, async, so a request is
   cancellable and times out; a response is data, and a status raises only with `check`.
   `gh` is REST and GraphQL over it: `repo`, `prs`, `pr` (files, reviews, checks), `issues`,
   `checks`, `api` (with `paginate`) and `graphql`. `TIMON_GITHUB_API` points it at GitHub
   Enterprise.
6. **`sqlite` (#92), built:** the bundled rusqlite. `sqlite.open(path)` (in memory without
   one) returns a database with `exec`, `query`, `one`, `last_id`, `transaction` and `close`.
   NULL reads as nil and blobs as Lua strings; parameters are a list for `?` or a table for
   `:name`. The signatures, from `help()`:

```lua
git.status{ cwd }        -- { branch, upstream, ahead, behind, clean, files }
git.log{ n, paths, since, range, cwd }   -- { { sha, short, author, email, date, subject, body } }
git.diff{ base, staged, stat, paths, cwd }    -- text, or { { path, added, removed } } with stat
git.add(paths)  git.commit{ message, all, paths, allow_empty }  git.push{ remote, ref, tags }
git.branch()  git.rev(ref)  git.root()  git.tag(name, { message })  git.tags{ pattern }
git.worktree_add{ path, branch, base }  git.worktrees()  git.worktree_remove(path, { force })
git.run(args, { cwd, input })            -- stdout, for everything else

gh.repo()  gh.prs{ state, base, limit }  gh.pr(12)  gh.issues{ state, labels, limit }
gh.checks(ref)  gh.graphql(query, variables)
gh.api("repos/{owner}/{repo}/pulls", { method = "POST", json = {...}, query, paginate, limit })

http.get(url, opts)  http.post(url, opts)  http.request{ method, url, ... }   -- a response

local db = sqlite.open("data.db")    -- sqlite.open() for an in-memory database
db:exec("create table if not exists seen (id text primary key)")
db:query("select * from seen where id = ?", { "x" })   -- { { id = "x" } }
```

Reads come first in `gh`; writes go through `gh.api` with an explicit method until common ones
earn a function. Git runs the `git` program rather than a Rust reimplementation, because pushes
and worktrees matter and the pure-Rust options do not cover them.

Later, also to discuss: a memory store, durable queues and workers, schedules, watch mode and
`.env` loading. Host hooks are built (see Hooks).

MCP servers fill the role babashka gives pods: external programs whose functions a script
calls. timon's MCP client (phase 9) makes any MCP server's tools Lua calls, so it needs no pod
protocol of its own.

## Phases

| Phase | Delivers | Done when |
|---|---|---|
| 0 | This design and a crate that evaluates plain Lua | merged |
| 1 | Runtime: eval, scripts, REPL, Fennel, `help`, errors, strict options and globals, `json`, `str`, `tbl`, `fs`, `scope`/`spawn`/`par`; directories and layers; `timon skill`; the plugin with the `timon` skill | a Claude Code session uses `timon eval` through the plugin for a day of real work |
| 2 | `agent`: run, ask, spawn, continue; supervisor; contracts and `schema`; agents; run records; `timon agent run`, `timon runs`; the workflow harness's base: phases, progress, nested budgets, script runs as records, resume | a fan-out review over three PRs and an author, review, follow-up cycle run as scripts; Ctrl-C leaves no child process |
| 3 | `sessions`: list, show, transcript, search, current, summarize; the index; `timon sessions` | the same hit sets as trovador for a fixed list of queries on one archive snapshot |
| 4 | Toolbox: `sh` and `proc` (#70) | this repository's gates run as a timon script, streaming output and exiting with the first failing gate's code |
| 5 | Tasks v1 (#68): `tasks.lua` in both layers, `timon tasks`, `timon <task>`, `depends`, tasks as script runs | this repository's gates, release steps and dogfood scripts are tasks in daily use, and an agent runs them through the plugin |
| 6 | Built-in tasks and typed options: timon's commands declared as tasks; options from the type syntax with `--help`, and `cli.parse` for scripts; `--jobs` for independent dependencies; `task.run`; exit codes (#39); `--dry-run`; shell completion | `timon tasks` lists built-in and project tasks alike, and every command's help comes from its declaration |
| 7 | The rest of the toolbox: `git`, `gh`, `http`, `sqlite`; data formats (#72); `eval` input modes (#71) | a release task (bump `Cargo.toml`, tag, push, GitHub release) written as a timon task |
| 8 | Workflow patterns: `flow` (#59) and its skill | a review with a verifier and a vote of three run as tasks |
| 9 | MCP: the `mcp` client, and `timon serve` with tasks as tools | an agent calls a project's tasks over MCP |
| 10 | Hooks (#104): `timon hook`, the session-start brief, the pre-tool guard, one `hooks.json` for both hosts; then memory, queues, schedules | a Claude Code and a Codex session get the brief and the guard through the plugin |

## Where code comes from

Copied, then adapted to timon's APIs. Each copy records its origin commit in the commit message.

| Need | Source |
|---|---|
| Supervisor, admission, cancellation, receipts | due, `spike/lua-composition` (`src/workflow`) |
| Fennel embedding | due's Fennel spike and finocchio's `compiler.rs` |
| Claude and Codex invocation and result parsing | due (`src/providers`) and tosi |
| Session archive readers and the FTS5 index | trovador (`archive.rs`, `codex.rs`, `transcript.rs`, `index.rs`) |
| Redaction | bottega-core (`redact.rs`) |
| Background processes, the toolbox's shape | babagent (design only) |

`claude-wrapper`, `codex-wrapper`, `tower-mcp` and `rusqlite` are crates.io dependencies: they
are libraries for exactly this, not sibling tools.

## What changes elsewhere

- trovador and due stay as they are and stay in use. timon does not read or write their state.
- babagent, voga, tosi, finocchio, lesto, bharli and squero stop. timon replaces them.

## Open questions

1. **Lua 5.5.** mlua 0.12.2 supports it. It needs a Fennel compatibility check before it is
   worth considering. Luau is settled for now (see Lua version).
2. **Binary size.** `json.query` uses jpx-core with every extension. Release binaries on macOS
   arm64: 2.47 MB without jpx-core, 2.84 MB with jpx-core and no extensions, 8.61 MB with all
   of them. Revisit when jpx-core splits its extensions into finer features.
   Measured 2026-10-16 on macOS arm64, clean release builds: 13.60 MB before `http` and `gh`;
   18.04 MB with reqwest, HTTP/2 and rustls on aws-lc-rs; 16.67 MB with rustls on ring; 16.05 MB
   on ring without HTTP/2 (#96). The rest of the HTTP stack (hyper, rustls, the platform
   verifier) is about 2.5 MB; a smaller client is the next candidate if size matters. The MCP
   client (tower-mcp 0.24, #100) adds about 2 MB: 18.04 MB.
3. **GitHub auth.** Decided and built (#90): `GH_TOKEN` or `GITHUB_TOKEN`, then `gh auth token`
   when `gh` is installed, otherwise `not_authenticated` with the fix. The `gh` fallback is a
   soft runtime dependency.
4. **MCP server import.** Whether `mcp.connect` can also use the servers Claude Code and Codex
   already have configured, or only entries in `mcp/`.
5. **Memory.** Whether a timon memory store replaces the local-context MCP server.
6. **Distribution.** `cargo install` only, or prebuilt binaries the plugin can fetch.
7. **A machine-wide concurrency cap** across separate timon invocations, or only per invocation.
8. **Codex hooks.** Decided (#104): Codex loads `hooks/hooks.json` from a plugin whose manifest
   is `.codex-plugin/plugin.json`, after the user reviews and trusts it, so the plugin's one file
   serves both hosts.
