---
name: timon-scripting
description: Use when writing timon scripts to keep - named scripts in ~/.config/timon or a project's .timon, shared lib modules, settings in init.lua, structured concurrency with scope, and Fennel. Load the timon skill first for the basics.
---

# Scripting with timon

## Scripts

A script is a `.lua` or `.fnl` file. Its arguments are in `arg` (`arg[1]`, ...) and `...`.

```lua fragment
#!/usr/bin/env timon
-- Print each argument doubled.
local out = {}
for i, a in ipairs(arg) do out[i] = tonumber(a) * 2 end
return out
```

```sh
timon double.lua 1 2 3   # [2,4,6]
```

## Named scripts and layers

Put scripts where `timon run` finds them. The same layout works for you and for a project:

```
~/.config/timon/      (everywhere)      <project>/.timon/   (one project)
├── init.lua          settings
├── scripts/NAME.lua  timon run NAME [ARGS]
├── lib/NAME.lua      require "NAME"
└── skills/NAME/      SKILL.md, references/
```

```sh
timon scripts             # what is in reach, with the first line of each script's comment
timon run triage --all    # runs scripts/triage.lua with "--all" in arg
timon defs                # layers, settings, scripts, lib directories
```

A project's `.timon/` loads only after a person reviews it and runs `timon trust`. Do not trust a
project on someone's behalf.

## Shared code in lib/

```lua
-- ~/.config/timon/lib/gh_util.lua
local M = {}
function M.ready(prs)
  return tbl.filter(prs, function(pr) return not pr.draft end)
end
return M
```

```lua fragment
-- any script
local util = require("gh_util")
```

## Settings

`init.lua` returns a table of settings and can do no I/O. Project settings override user
settings key by key.

```lua
-- ~/.config/timon/init.lua
return {
  defaults = { provider = "claude", model = "opus" },
  limits = { concurrency = 4 },
}
```

## Concurrency

`scope(fn)` runs `fn(s)`; tasks spawned on `s` finish before it returns. If any task raises,
the others are cancelled and the error is raised.

```lua
local a, b = scope(function(s)
  local ha = s:spawn(function() sleep("20ms") return "slow" end)
  local hb = s:spawn(function() return "fast" end)
  return ha:await(), hb:await()
end)
return a, b  -- slow, fast
```

Keep going past failures with `settle`:

```lua
local results = par.map({ 1, 2, 3 }, function(n)
  if n == 2 then error({ kind = "bad_input", message = "two" }) end
  return n * 10
end, { settle = true })
return tbl.map(results, function(r) return r.ok and r.value or r.error.kind end)
-- [10,"bad_input",30]
```

`par.race` returns the first result and cancels the rest:

```lua
local value, i = par.race(
  function() sleep(10) return "slow" end,
  function() sleep("5ms") return "fast" end
)
return value, i  -- fast, 2
```

## Fennel

Every module works from Fennel. Use `.fnl` files, `timon eval --fnl`, or `require` a `.fnl`
module.

```fennel
(local prs [{:number 12 :draft false} {:number 14 :draft true}])
(-> (tbl.filter prs (fn [pr] (not pr.draft)))
    (tbl.map (fn [pr] pr.number)))
```

## Errors in scripts

Raise structured errors so callers can branch on them:

```lua
local function load_config(file)
  if not fs.exists(file) then
    error({ kind = "missing_config", message = file .. " does not exist", hint = "run setup first" })
  end
  return json.decode(fs.read(file))
end
local ok, err = try(load_config, "/no/config.json")
return err.kind, err.hint  -- missing_config, run setup first
```
