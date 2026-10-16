# timon API

Generated from the same declarations as `help()`.

## Globals

Functions available everywhere without a module prefix.

### `help(topic) -> string`

Describes a module or function. Pass the value itself or its name; no argument lists the modules.

```lua
help(json)
help(json.encode)
help("str.fmt")
```

### `try(f, ...) -> true, ... | false, err`

Calls f with the arguments. Returns true and its results, or false and the error as a table with kind, message and hint.

```lua
local ok, err = try(fs.read, "missing.txt")
if not ok then print(err.kind) end  --> not_found
```

### `pp(...) -> ...`

Pretty-prints each value to stderr and returns them unchanged, so it can wrap an expression.

```lua
local prs = pp(json.decode(text))
```

### `scope(fn) -> ...`

Runs fn(s) and returns its results once every task spawned on s has finished. If fn or any task raises, the others are cancelled and the first error is raised. s:spawn(f, ...) starts a task; s:cancel() cancels the running ones. scope() with no function returns a scope for local s <close> = scope(), which waits when the block ends.

```lua
local review, history = scope(function(s)
  local r = s:spawn(agent.run, { agent = "reviewer", prompt = "Review PR 12" })
  local h = s:spawn(sessions.search, { query = "PR 12" })
  return r:await(), h:await()
end)
```

### `spawn(f, ...) -> task`

Starts f(...) as a task on the innermost scope; a script's main chunk is one, and the script waits for its tasks. task:await() returns the results or raises the error, task:cancel() stops it, task:done() tells whether it finished.

```lua
local h = spawn(fs.read, "big.json")
-- other work
local text = h:await()
```

### `sleep(duration)`

Waits without blocking other tasks. A number is seconds; a string takes ms, s, m or h.

```lua
sleep("250ms")
sleep(2)
```

## json

Encode and decode JSON. json.null stands for null.

### `json.encode(value, { pretty }) -> string`

Encodes a value. A table with keys 1..n is an array, any other table an object with sorted keys. Functions, cycles and arrays with holes are errors.

| Option | Type | |
|---|---|---|
| `pretty` | boolean | indent with two spaces |

```lua
json.encode({ name = "timon", tags = { "lua", "agents" } })
--> {"name":"timon","tags":["lua","agents"]}
```

### `json.decode(text) -> value`

Decodes JSON text. Arrays and objects keep their shape when encoded again, even when empty; null becomes json.null.

```lua
local data = json.decode('{"prs": [12, 14]}')
return data.prs[2]  --> 14
```

### `json.array(t) -> t`

Marks t, or a new table, as an array so it encodes as [] when empty.

```lua
json.encode({ items = json.array{} })  --> {"items":[]}
```

### `json.query(value, expression) -> value`

Evaluates a JMESPath expression against a value, with jpx's extension functions (strings, dates, math, hashing and more). Lists come back as arrays and null as json.null.

```lua
json.query(prs, "[?draft == `false`].number")
json.query(data, "items[*].name | sort(@) | join(', ', @)")
json.query(run, "upper(status)")
```

### `json.object(t) -> t`

Marks t, or a new table, as an object so it encodes as {} when empty.

```lua
json.encode(json.object{})  --> {}
```

## str

String helpers. Each is also a string method: s:split(","), s:trim().

### `str.split(s, sep) -> array`

Splits on a plain separator (no patterns). Without one, splits on runs of whitespace and drops empty pieces.

```lua
str.split("a,b,,c", ",")  --> { "a", "b", "", "c" }
("  one two "):split()     --> { "one", "two" }
```

### `str.trim(s) -> string`

Removes leading and trailing whitespace.

```lua
("  hi \n"):trim()  --> "hi"
```

### `str.lines(s) -> array`

Splits into lines, accepting \n and \r\n. A trailing newline does not add an empty line.

```lua
for i, line in ipairs(str.lines(fs.read("notes.txt"))) do print(i, line) end
```

### `str.starts(s, prefix) -> boolean`

Whether s begins with prefix.

```lua
("feat: x"):starts("feat:")  --> true
```

### `str.ends(s, suffix) -> boolean`

Whether s ends with suffix.

```lua
("review.lua"):ends(".lua")  --> true
```

### `str.fmt(template, vars) -> string`

Fills ${name} placeholders from vars. Strings are inserted as they are, tables as JSON, other values with tostring. $${ writes a literal ${. A missing name is an error.

```lua
str.fmt("Review PR ${n}: ${files}", { n = 12, files = { "a.rs" } })
--> Review PR 12: ["a.rs"]
```

## tbl

Table helpers. Results that are lists are arrays; callbacks get (value, index).

### `tbl.map(t, f) -> array`

Calls f(v, i) for each element and collects the results, skipping nils.

```lua
tbl.map({ 1, 2, 3 }, function(n) return n * n end)  --> { 1, 4, 9 }
```

### `tbl.filter(t, f) -> array`

The elements for which f(v, i) is truthy.

```lua
tbl.filter(prs, function(pr) return pr.draft == false end)
```

### `tbl.reduce(t, f, init) -> value`

Folds left with f(acc, v, i). Without init, starts from the first element.

```lua
tbl.reduce({ 1, 2, 3 }, function(sum, n) return sum + n end, 0)  --> 6
```

### `tbl.find(t, f) -> v, i`

The first element for which f(v, i) is truthy, and its index; nil when none is.

```lua
local pr = tbl.find(prs, function(p) return p.number == 12 end)
```

### `tbl.keys(t) -> array`

The keys, sorted (numbers before strings).

```lua
tbl.keys({ b = 1, a = 2 })  --> { "a", "b" }
```

### `tbl.values(t) -> array`

The values, in the order of tbl.keys.

```lua
tbl.values({ b = 1, a = 2 })  --> { 2, 1 }
```

### `tbl.group_by(t, f) -> table`

Groups elements by f(v, i); each group is an array. Elements whose key is nil are dropped.

```lua
tbl.group_by(findings, function(f) return f.severity end).high
```

### `tbl.sort_by(t, f) -> array`

A new array sorted by f(v, i), stable for equal keys. t is not changed.

```lua
tbl.sort_by(prs, function(p) return p.updated_at end)
```

### `tbl.count(t, f) -> integer`

How many elements satisfy f(v, i); without f, how many entries t has.

```lua
tbl.count(reviews, function(r) return r.status == "ok" end)
```

### `tbl.merge(...) -> table`

A new table with the entries of each argument; later ones win. nil arguments are skipped.

```lua
tbl.merge(defaults, { model = "opus" })
```

### `tbl.pick(t, keys) -> table`

A new table with only the given keys.

```lua
tbl.pick(pr, { "number", "title" })
```

### `tbl.omit(t, keys) -> table`

A new table without the given keys.

```lua
tbl.omit(run, { "events" })
```

## fs

Files and directories. Paths are relative to the working directory.

### `fs.read(path) -> string`

The file's contents. A missing file raises not_found.

```lua
local text = fs.read("README.md")
```

### `fs.write(path, data, { append, mkdir })`

Writes data to the file, replacing it unless append is set.

| Option | Type | |
|---|---|---|
| `append` | boolean | add to the end instead of replacing |
| `mkdir` | boolean | create missing parent directories |

```lua
fs.write("out/report.json", json.encode(report), { mkdir = true })
```

### `fs.exists(path) -> boolean`

Whether anything exists at path.

```lua
if not fs.exists(".timon") then fs.mkdir(".timon") end
```

### `fs.ls(dir, { hidden }) -> array`

Entry names in dir (default "."), sorted. Names starting with a dot are left out unless hidden is set.

| Option | Type | |
|---|---|---|
| `hidden` | boolean | include names starting with a dot |

```lua
for _, name in ipairs(fs.ls("src")) do print(name) end
```

### `fs.glob(pattern) -> array`

Paths matching a shell pattern (*, ?, [ab], ** for any depth), sorted.

```lua
fs.glob("src/**/*.rs")
```

### `fs.mkdir(path)`

Creates a directory and any missing parents. An existing directory is fine.

```lua
fs.mkdir("out/logs")
```

### `fs.tmpdir() -> string`

Creates a new empty directory under the system temp directory and returns its path. It is not removed automatically.

```lua
local dir = fs.tmpdir()
fs.write(path.join(dir, "x.txt"), "hi")
```

### `fs.cwd() -> string`

The working directory.

```lua
fs.cwd()
```

### `fs.home() -> string`

The home directory.

```lua
path.join(fs.home(), ".config", "timon")
```

## path

Path names, without touching the filesystem.

### `path.join(...) -> string`

Joins parts with the platform separator. An absolute part replaces what came before.

```lua
path.join("src", "modules", "fs.rs")  --> "src/modules/fs.rs"
```

### `path.base(p) -> string`

The last component, or "" when there is none.

```lua
path.base("src/main.rs")  --> "main.rs"
```

### `path.dir(p) -> string`

Everything before the last component, or "." when there is nothing before it.

```lua
path.dir("src/main.rs")  --> "src"
```

### `path.ext(p) -> string`

The extension without the dot, or "".

```lua
path.ext("review.lua")  --> "lua"
```

## log

Messages on stderr, so stdout stays parseable.

### `log.info(...)`

Writes "info: " and the values, separated by spaces. Tables print as JSON.

```lua
log.info("reviewing", #prs, "PRs")
```

### `log.warn(...)`

Writes "warn: " and the values.

```lua
log.warn("no tests found in", dir)
```

### `log.error(...)`

Writes "error: " and the values. It does not raise; use error() to stop.

```lua
log.error("review failed:", run.error.message)
```

## par

Run functions concurrently. Each call is a scope: an error cancels the rest and is raised.

### `par.map(items, f, { limit, settle }) -> array`

Calls f(item, i) for every item, at most limit at a time, and returns the results in input order. With settle, failures do not cancel the rest: each result is { ok = true, value = v } or { ok = false, error = err }.

| Option | Type | |
|---|---|---|
| `limit` | integer | most calls running at once (default: all) |
| `settle` | boolean | collect failures as results instead of raising |

```lua
local reviews = par.map(prs, function(pr)
  return agent.run{ agent = "reviewer", prompt = "Review PR " .. pr }
end, { limit = 3 })
```

### `par.all(f1, f2, ...) -> r1, r2, ...`

Runs the functions concurrently and returns the first result of each, in order.

```lua
local log, status = par.all(
  function() return sh("git log -5") end,
  function() return sh("git status") end
)
```

### `par.race(f1, f2, ...) -> result, i`

Runs the functions concurrently, returns the first result to finish and its position, and cancels the others.

```lua
local answer, i = par.race(
  function() return agent.ask{ provider = "claude", prompt = q } end,
  function() return agent.ask{ provider = "codex", prompt = q } end
)
```

## schema

Shapes for structured data. Values: schema.string, schema.int, schema.number, schema.bool, schema.any. Every object field is required unless optional; s:describe(text) documents a field. A contract is a named schema in contracts/.

### `schema.object(fields) -> schema`

An object with exactly these fields. Each field is required unless wrapped in schema.optional.

```lua
schema.object{
  verdict = schema.enum{ "approve", "request_changes" },
  summary = schema.string:describe("two sentences at most"),
  notes = schema.optional(schema.string),
}
```

### `schema.array(item) -> schema`

A list whose elements all match item.

```lua
schema.array(schema.object{ file = schema.string, line = schema.int })
```

### `schema.map(value) -> schema`

An object with any string keys whose values all match value. Codex's strict output mode does not accept it; use object for agent output.

```lua
schema.map(schema.int)  -- { a = 1, b = 2 }
```

### `schema.enum(values) -> schema`

One of the given strings.

```lua
schema.enum{ "high", "medium", "low" }
```

### `schema.optional(s) -> schema`

s, or missing, or null.

```lua
schema.object{ name = schema.string, nickname = schema.optional(schema.string) }
```

### `schema.check(s, value) -> true | false, errors`

Validates value. Each error is { path, message }, with paths like findings[2].line.

```lua
local ok, errors = schema.check(schema.object{ n = schema.int }, { n = "x" })
-- false, { { path = "n", message = "expected an integer, got a string" } }
```

### `schema.json(s) -> table`

The JSON Schema for s, as given to providers for structured output.

```lua
json.encode(schema.json(schema.contract("review")), { pretty = true })
```

### `schema.contract(name) -> schema`

A named schema: contracts/NAME.lua in the project or user layer, or a built-in one (review, implementation, investigation, yes_no, list).

```lua
local review = schema.contract("review")
```

## agent

Claude Code and Codex runs. agent.run returns a record and never raises for an outcome; agent.ask returns the answer or raises. Runs spend tokens.

### `agent.run{ prompt, agent, provider, model, effort, access, ... } -> record`

Runs one non-interactive turn. The record has id, status (ok, failed, invalid, timeout, cancelled), answer (with output), text, error, provider, model, session, usage { input_tokens, output_tokens, usd } and ms. Fields merge: defaults < init.lua defaults < the agent definition < this call.

| Option | Type | |
|---|---|---|
| `prompt` | string | what to do (required) |
| `agent` | string | a definition from agents/NAME.lua to start from |
| `provider` | string | "claude" (default) or "codex" |
| `model` | string | the provider's model name |
| `effort` | string | claude: low, medium, high, xhigh, max; codex: passed through |
| `access` | string | "read" (default), "write", or "full" (needs limits.allow_full) |
| `tools` | table | claude's tool list, replacing the one access picks |
| `cwd` | string | the directory to run in |
| `system` | string | instructions added to the system prompt |
| `output` | any | a contract name or a schema; the answer is validated |
| `timeout` | any | seconds, or a duration such as "10m" |
| `budget` | table | { usd = number, turns = integer } (claude) |
| `resume` | string | a session id timon started, to continue |
| `fork` | string | a session id to branch from (claude) |
| `native` | table | extra command-line arguments for the provider |

```lua
local r = agent.run{ prompt = "Review the diff on this branch", output = "review" }
if r.status == "ok" then pp(r.answer.findings) else log.error(r.error.message) end
```

### `agent.ask(prompt | spec) -> answer | text`

Runs like agent.run and returns the answer (with output) or the text. Raises agent_failed, with the record as error.data, when the run is not ok.

```lua
local ok = agent.ask{ prompt = "Is this function pure? " .. src, output = "yes_no" }
if ok.answer then print(ok.reason) end
```

### `agent.spawn(spec) -> task`

Starts agent.run(spec) as a task on the innermost scope; task:await() returns the record.

```lua
local a = agent.spawn{ prompt = "Summarize README.md" }
local b = agent.spawn{ prompt = "Summarize CHANGELOG.md", provider = "codex" }
local ra, rb = a:await(), b:await()
```

### `agent.runs{ since, limit } -> array`

Recorded runs, newest first: id, started_at, provider, model, agent, status, ms, usd, output_tokens and the start of the prompt. timon runs show ID has the full request and result.

| Option | Type | |
|---|---|---|
| `since` | any | a duration back from now, such as "1d" or "2h" |
| `limit` | integer | most entries to return (default 20) |

```lua
for _, r in ipairs(agent.runs{ since = "1d" }) do print(r.id, r.status, r.prompt) end
```

### `agent.continue(record, prompt, spec) -> record`

Resumes record's session with a new prompt, on the same provider. spec adds or overrides fields.

```lua
local r = agent.run{ prompt = "List the failing tests" }
local fix = agent.continue(r, "Fix the first one", { access = "write" })
```
