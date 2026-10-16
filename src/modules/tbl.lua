-- tbl: table helpers, written in Lua. Loaded by tbl.rs with the json.array marker and a
-- function that builds error tables.
local array, fail = ...
local type, ipairs, pairs, select, tostring, sort = type, ipairs, pairs, select, tostring, table.sort

local function want(name, value, kind, pos)
  if type(value) ~= kind then
    error(fail("bad_argument",
      ("%s: argument %d must be a %s, not %s"):format(name, pos, kind, type(value))), 3)
  end
end

local function order(a, b)
  local ta, tb = type(a), type(b)
  if ta ~= tb then return ta < tb end
  if ta == "number" or ta == "string" then return a < b end
  return tostring(a) < tostring(b)
end

local function sorted_keys(t)
  local keys = {}
  for k in pairs(t) do keys[#keys + 1] = k end
  sort(keys, order)
  return keys
end

local tbl = {}

function tbl.map(t, f)
  want("tbl.map", t, "table", 1)
  want("tbl.map", f, "function", 2)
  local out = array()
  for i, v in ipairs(t) do
    local r = f(v, i)
    if r ~= nil then out[#out + 1] = r end
  end
  return out
end

function tbl.filter(t, f)
  want("tbl.filter", t, "table", 1)
  want("tbl.filter", f, "function", 2)
  local out = array()
  for i, v in ipairs(t) do
    if f(v, i) then out[#out + 1] = v end
  end
  return out
end

function tbl.reduce(t, f, init)
  want("tbl.reduce", t, "table", 1)
  want("tbl.reduce", f, "function", 2)
  local acc, start = init, 1
  if acc == nil then acc, start = t[1], 2 end
  for i = start, #t do acc = f(acc, t[i], i) end
  return acc
end

function tbl.find(t, f)
  want("tbl.find", t, "table", 1)
  want("tbl.find", f, "function", 2)
  for i, v in ipairs(t) do
    if f(v, i) then return v, i end
  end
  return nil
end

function tbl.keys(t)
  want("tbl.keys", t, "table", 1)
  return array(sorted_keys(t))
end

function tbl.values(t)
  want("tbl.values", t, "table", 1)
  local out = array()
  for _, k in ipairs(sorted_keys(t)) do out[#out + 1] = t[k] end
  return out
end

function tbl.group_by(t, f)
  want("tbl.group_by", t, "table", 1)
  want("tbl.group_by", f, "function", 2)
  local out = {}
  for i, v in ipairs(t) do
    local key = f(v, i)
    if key ~= nil then
      local group = out[key]
      if group == nil then
        group = array()
        out[key] = group
      end
      group[#group + 1] = v
    end
  end
  return out
end

function tbl.sort_by(t, f)
  want("tbl.sort_by", t, "table", 1)
  want("tbl.sort_by", f, "function", 2)
  local decorated = {}
  for i, v in ipairs(t) do decorated[i] = { key = f(v, i), index = i, value = v } end
  sort(decorated, function(a, b)
    if a.key == b.key then return a.index < b.index end
    return order(a.key, b.key)
  end)
  local out = array()
  for i, d in ipairs(decorated) do out[i] = d.value end
  return out
end

function tbl.count(t, f)
  want("tbl.count", t, "table", 1)
  local n = 0
  if f == nil then
    for _ in pairs(t) do n = n + 1 end
    return n
  end
  want("tbl.count", f, "function", 2)
  for i, v in ipairs(t) do
    if f(v, i) then n = n + 1 end
  end
  return n
end

function tbl.merge(...)
  local out = {}
  for i = 1, select("#", ...) do
    local t = select(i, ...)
    if t ~= nil then
      want("tbl.merge", t, "table", i)
      for k, v in pairs(t) do out[k] = v end
    end
  end
  return out
end

function tbl.pick(t, keys)
  want("tbl.pick", t, "table", 1)
  want("tbl.pick", keys, "table", 2)
  local out = {}
  for _, k in ipairs(keys) do out[k] = t[k] end
  return out
end

function tbl.omit(t, keys)
  want("tbl.omit", t, "table", 1)
  want("tbl.omit", keys, "table", 2)
  local skip = {}
  for _, k in ipairs(keys) do skip[k] = true end
  local out = {}
  for k, v in pairs(t) do
    if not skip[k] then out[k] = v end
  end
  return out
end

return tbl
