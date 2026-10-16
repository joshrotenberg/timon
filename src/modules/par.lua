-- par: concurrency helpers on top of scope. Loaded by concurrency.rs.
local scope, try, array, check, first, fail = ...
local type, pack, unpack, min, huge = type, table.pack, table.unpack, math.min, math.huge

local function want(name, value, kind, pos)
  if type(value) ~= kind then
    error(fail("bad_argument",
      ("bad argument #%d to %s: expected a %s, got a %s"):format(pos, name, kind, type(value))), 3)
  end
end

local function functions(name, ...)
  local fns = pack(...)
  for i = 1, fns.n do want(name, fns[i], "function", i) end
  return fns
end

local par = {}

function par.map(items, f, opts)
  want("par.map", items, "table", 1)
  want("par.map", f, "function", 2)
  check("par.map", opts)
  local limit = opts and opts.limit or huge
  local settle = opts and opts.settle or false
  if limit < 1 then
    error(fail("bad_option", "option \"limit\" to par.map must be at least 1"), 2)
  end
  local n = #items
  local results = array()
  local next_index = 1
  scope(function(s)
    local function worker()
      while next_index <= n do
        local i = next_index
        next_index = i + 1
        if settle then
          local r = pack(try(f, items[i], i))
          if r[1] then
            results[i] = { ok = true, value = r[2] }
          else
            results[i] = { ok = false, error = r[2] }
          end
        else
          results[i] = f(items[i], i)
        end
      end
    end
    for _ = 1, min(limit, n) do s:spawn(worker) end
  end)
  return results
end

function par.all(...)
  local fns = functions("par.all", ...)
  return scope(function(s)
    local handles, out = {}, {}
    for i = 1, fns.n do handles[i] = s:spawn(fns[i]) end
    for i = 1, fns.n do out[i] = (handles[i]:await()) end
    return unpack(out, 1, fns.n)
  end)
end

function par.race(...)
  local fns = functions("par.race", ...)
  return scope(function(s)
    local handles = {}
    for i = 1, fns.n do handles[i] = s:spawn(fns[i]) end
    local i = first(handles)
    local value = handles[i]:await()
    s:cancel()
    return value, i
  end)
end

return par
