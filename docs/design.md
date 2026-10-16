# timon design

Status: design. Nothing here is implemented unless it says so. `README.md` is canonical for what
exists; this document is canonical for direction.

timon (Venetian for rudder) is one self-contained binary that gives an agent, or a person, an
embedded language and a set of clean APIs to use it around: running Claude and Codex as
subagents, reading every past session, calling any MCP server, and a toolbox for git, JSON, HTTP,
GitHub and SQLite. Code is evaluated from the command line or written as scripts. The skills that
teach it are bundled in the binary, and it ships as a plugin for Claude Code and Codex.

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

timon takes the parts that worked and drops the parts that did not. The language is Lua, stock.
Fennel comes with it, stock. All the effort goes into the APIs.

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
    (`limits.concurrency = 4`), features and their cost are chosen for the small case.

## Shape

```
timon (one binary)
├─ runtime     Lua 5.4 (mlua, vendored, async) and fennel.lua (pinned, stock)
│              eval, scripts, REPL, help(), errors, scope/spawn/par, json, str, tbl, fs
├─ agent       Claude and Codex runs: run, ask, spawn; contracts; agent definitions
├─ supervisor  concurrency caps, budgets, cancellation, run records
├─ sessions    every Claude Code and Codex session: list, show, transcript, search
├─ mcp         client: any MCP server's tools as Lua calls; server: `timon serve`
├─ toolbox     git, gh, http, sh, proc, sqlite              (to discuss, see Toolbox)
├─ later       hooks, memory, queues                          (to discuss)
├─ storage     SQLite (bundled, FTS5): state, run records, the session index
├─ surfaces    CLI, scripts, REPL, MCP server; hooks later
└─ embedded    skill library, LuaLS type stubs, plugin templates
```

A person's own agents, contracts, skills and scripts live in `~/.config/timon/`, a project's in
`.timon/`, in the same format (see Directories).

## API conventions

These hold for every module, so an agent that has used one module can guess the next.

- **Modules are globals.** `agent`, `sessions`, `json`, `fs` and the rest are global tables in
  every script and in `eval`. `require "agent"` returns the same table.
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
  option declarations behind strict options are the same source, and they also generate the LuaLS stubs and the reference sections of the
  skills, so the checks, the help, the stubs and the skills cannot drift apart.

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
`...`. `timon run NAME [ARGS]` runs a named script from a `scripts/` directory (see Directories).
A script's last returned value is printed the way `eval` prints.

### REPL

`timon repl` (and `timon repl --fnl`) with history, completion of module and field names from
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

The pinned `fennel.lua` loads on first use. `.fnl` files are accepted wherever `.lua` files are:
scripts, `eval --fnl`, `repl --fnl` and every definition directory. Fennel code calls the same
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
| globals | `help`, `try`, `scope`, `spawn`, `par`, `sleep`, `pp` (pretty-print to stderr), `log.info/warn/error` |

The stock Lua standard library stays intact.

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
| `read` | read-only tool set (Read, Grep, Glob and safe Bash reads) | `--sandbox read-only` |
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
`any` and `:describe`. `schema.json(s)` gives JSON Schema 2020-12, which is what both providers
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
- **Script runs and resume.** `timon run NAME` and `timon FILE` are script runs, recorded
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
sessions.list{ backend = "claude", project = ".", since = "7d", limit = 20 }
-- { { id, backend, title, project, cwd, modified, size_bytes, origin, resumable, prs }, ... }

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
- Transcript text is redacted (`[redacted:<kind>]`) and untrusted.
- Summaries run through `agent.run` (`ephemeral`, no tools, one turn), so budgets and run
  records apply. The transcript is redacted before it is sent; a long one is summarized in
  parts and merged. The cache is keyed by the session file's size and modification time.
- The CLI door mirrors the module, plain text by default and JSON with `--json`:
  `timon sessions search mlua --since 30d`.

## mcp

timon is an MCP client and an MCP server, both through tower-mcp.

### Client

Any MCP server's tools become Lua calls:

```lua
local crates = mcp.connect("crates")             -- an entry in mcp/, by name
local files = mcp.connect{ command = { "npx", "-y", "@modelcontextprotocol/server-filesystem", "." } }
local remote = mcp.connect{ url = "https://example.com/mcp", headers = { ... } }

crates:tools()                                   -- { { name, description, input_schema } }
crates:call("get_crate_info", { name = "mlua" }) -- the structured content, or the text
crates:resources()                               -- { { uri, name, mime_type } }
crates:read("crates://mlua/readme")
crates:close()                                   -- also when the opening scope ends
```

- `call` raises when the tool reports an error, with the server's message in the error; `try`
  catches it.
- A connection belongs to the scope that opened it, like spawned work: it closes when that scope
  ends, and the supervisor stops the server process.
- `mcp.servers()` lists the configured entries.
- The CLI door: `timon mcp tools crates`, `timon mcp call crates get_crate_info name=mlua`.

### Server

`timon serve` runs an MCP server over stdio for hosts that prefer tools to a shell, or have no
shell:

| Tool | Does |
|---|---|
| `eval` | Evaluates Lua or Fennel and returns the result as JSON |
| `scripts` | Lists the scripts in reach, with descriptions and parameter schemas |
| `run_script` | Runs a script by name with arguments |
| `sessions_search`, `sessions_transcript` | The sessions module, for hosts that call tools only |

A script with typed parameters can also be exposed as its own tool by naming it in `init.lua`
(`serve = { expose = { "triage" } }`), so a host sees a typed tool instead of a generic runner.
The skills are published as MCP resources with the same text, so a client without plugin support
reads the same guidance.

## Directories

Two layers share one format, over what is built into the binary:

| Layer | Where | Holds |
|---|---|---|
| built in | the binary | the skills, contracts and agents that ship with timon |
| user | `~/.config/timon/` (`$XDG_CONFIG_HOME/timon`) | a person's own definitions, available everywhere |
| project | `.timon/` in the nearest ancestor directory that has one | the same, for one project, usually committed |

```
~/.config/timon/  and  <project>/.timon/
├── init.lua              settings for this layer: defaults, limits, serve (optional)
├── agents/
│   └── reviewer.lua      one agent per file; the file name is the name
├── contracts/
│   └── review.lua        one schema per file
├── skills/
│   └── release/
│       ├── SKILL.md      what Claude Code and Codex read
│       ├── skill.lua     optional: makes it runnable with `timon skill run release`
│       └── references/   optional: sections `timon skill show release --ref NAME` prints
├── scripts/
│   └── triage.lua        `timon run triage [ARGS]`
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
- `timon defs` lists every agent, contract, skill, script and MCP server with the layer it came
  from and what it shadows.

### Files

```lua
-- init.lua
return {
  defaults = { provider = "claude", model = "opus", effort = "medium", access = "read" },
  limits   = { concurrency = 4, max_usd = 10 },
  serve    = { expose = { "triage" } },
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
return { command = { "cratesio-mcp" } }      -- or { url = "http://127.0.0.1:8080/mcp" }
```

```lua
-- scripts/triage.lua
-- Label every unlabeled issue in this repository.
return {
  params = schema.object{ limit = schema.optional(schema.int) },
  run = function(p)
    ...
  end,
}
```

- **Definitions load with no effects.** `init.lua` and the files in `agents/`, `contracts/`,
  `mcp/` and `skill.lua` run in an environment with the pure standard library, `str`, `tbl` and
  `json` (and `schema` once contracts exist), and no I/O. Each must return a table, which is validated; an error names the file and
  the field. Settings in `init.lua` merge key by key, project over user over built in.
- **Scripts are programs.** A script runs with every module available. Its description for
  `timon scripts` is its leading `--` comment, read without running it. A script that returns
  `{ params = schema, run = function(p) ... end }` gets typed parameters, parsed from flags on
  the command line and from arguments over MCP; any other script gets `arg`.
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
timon skill show timon-agents --ref fan-out
```

| Skill | Teaches |
|---|---|
| `timon` | What timon is, `eval` and scripts, the conventions above, the module index, errors, untrusted text, cost |
| `timon-agents` | `agent.run`, contracts, fan-out, author and review loops, worktrees, budgets |
| `timon-sessions` | Finding earlier work before answering from memory |
| `timon-scripting` | Scripts, directories, layers, trust, Fennel |

Each skill is short and links reference sections that `timon skill show` prints on demand,
because context is the scarce resource. A skill joins the library with the module it teaches.
The plugin ships the same files, generated from the binary, so a host with plugin support and
one without read identical guidance.

## Plugin

The repository is its own marketplace:

```sh
claude plugin marketplace add joshrotenberg/timon
claude plugin install timon@timon
```

- `plugin/` holds both hosts' manifests (`.claude-plugin/plugin.json` for Claude Code,
  `plugin.json` for Codex), the skills and a `bin/timon` shim. Once `timon serve` exists, both
  hosts' MCP manifests register it.
- `timon plugin build plugin/` generates the directory from the embedded skills and checked-in
  templates; `timon plugin build --check plugin/` fails when they differ, and CI runs it.
- The shim runs the installed binary (`cargo install --path .`). Prebuilt binaries are an open
  question.
- Rules carried over from bottega: no `version` fields, a relative `./bin/timon` command in
  Codex's `mcp.json`, and no `hooks/hooks.json`, because Codex reads that path too.

## Toolbox (to discuss)

These follow the agent and sessions work. The shapes below are starting points for that
discussion, not commitments.

```lua
git.status()             -- { branch, upstream, ahead, behind, files = { { path, status } } }
git.log{ n = 10, path = "src", since = "2w" }   -- { { sha, author, date, subject, body } }
git.diff{ base = "main", stat = true }
git.worktree.add{ branch = "x" }   git.commit{ message = "..." }   git.run{ "rev-parse", "HEAD" }

gh.repo()                gh.prs{ state = "open" }       gh.pr(12)   -- with files, checks, reviews
gh.issues{ labels = { "bug" } }    gh.checks("HEAD")
gh.api("repos/{owner}/{repo}/pulls", { method = "POST", body = {...}, paginate = true })
gh.graphql(query, vars)


http.get(url, { headers = {...} })   -- { status, headers, body, json = function }
http.post(url, { json = {...} })

sh("git rev-parse HEAD")             -- stdout, trimmed; raises on a nonzero exit
proc.run{ "cargo", "test", cwd = "." }   -- { code, out, err, ms }; never raises on exit codes
proc.spawn{ name = "server", "cargo", "run" }   -- background process with logs, stop, wait

local db = sqlite.open("data.db")    -- sqlite.open() for an in-memory database
db:exec("create table if not exists seen (id text primary key)")
db:query("select * from seen where id = ?", { "x" })   -- { { id = "x" } }
```

Reads come first in `gh`; writes go through `gh.api` with an explicit method until common ones
earn a function. Git runs the `git` program rather than a Rust reimplementation, because pushes
and worktrees matter and the pure-Rust options do not cover them.

Later, also to discuss: host hooks (`timon hook session-start` injecting a brief), a memory
store, durable queues and workers, and schedules.

## Phases

| Phase | Delivers | Done when |
|---|---|---|
| 0 | This design and a crate that evaluates plain Lua | merged |
| 1 | Runtime: eval, scripts, REPL, Fennel, `help`, errors, strict options and globals, `json`, `str`, `tbl`, `fs`, `scope`/`spawn`/`par`; directories and layers; `timon skill`; the plugin with the `timon` skill | a Claude Code session uses `timon eval` through the plugin for a day of real work |
| 2 | `agent`: run, ask, spawn, continue; supervisor; contracts and `schema`; agents; run records; `timon agent run`, `timon runs` | a fan-out review over three PRs and an author, review, follow-up cycle run as scripts; Ctrl-C leaves no child process |
| 3 | `sessions`: list, show, transcript, search, current, summarize; the index; `timon sessions` | the same hit sets as trovador for a fixed list of queries on one archive snapshot |
| 4 | Toolbox and MCP: git, gh, http, sh, proc, sqlite; the `mcp` client and `timon serve` | after the discussion above |
| 5 | Hooks, memory, queues | after phase 4 is in daily use |

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
2. **JMESPath size.** `json.query` uses jpx-core with every extension. Release binaries on macOS
   arm64: 2.47 MB without jpx-core, 2.84 MB with jpx-core and no extensions, 8.61 MB with all
   of them. Revisit when jpx-core splits its extensions into finer features.
3. **GitHub auth.** `GH_TOKEN` or `GITHUB_TOKEN`, then `gh auth token` when `gh` is installed,
   otherwise an error with the fix. The `gh` fallback is a soft runtime dependency.
4. **MCP server import.** Whether `mcp.connect` can also use the servers Claude Code and Codex
   already have configured, or only entries in `mcp/`.
5. **Memory.** Whether a timon memory store replaces the local-context MCP server.
6. **Distribution.** `cargo install` only, or prebuilt binaries the plugin can fetch.
7. **A machine-wide concurrency cap** across separate timon invocations, or only per invocation.
8. **Codex hooks.** Codex loads no hooks from plugins, so a Codex hook is user configuration that
   the README documents.
