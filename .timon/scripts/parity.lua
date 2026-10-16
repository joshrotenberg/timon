-- Compare session search with trovador's on this machine: the same sessions, the same match counts.
-- timon parity [QUERY ...]. Needs trovador on PATH; reads the archives and changes nothing.
local queries = { "redismodule", "babashka task runner", "mlua async", "jpx", "worktree", "fennel", "tower-mcp", '"cargo test"' }
if #arg > 0 then queries = { table.unpack(arg) } end

-- Sessions written in the last ten minutes can change between the two searches.
local settled = os.date("!%Y-%m-%dT%H:%M:%S", os.time() - 600)

local function counts(hits, id_field)
  local out, n = {}, 0
  for _, h in ipairs(hits) do
    if h.modified < settled then
      out[h[id_field]] = h.match_count
      n = n + 1
    end
  end
  return out, n
end

local differ = {}
for _, q in ipairs(queries) do
  local mine = sessions.search{ query = q, backend = "claude", limit = 10000, per_session = 1,
    automated = true, subagents = true, active = true, exclude = false }
  local ok, run = try(proc.run, { "trovador", "search", q, "--backend", "claude", "--limit", "10000",
    "--per-session", "1", "--include-automated", "--include-subagents", "--include-active" })
  if not ok then
    error{ kind = "not_found", message = "parity needs trovador on PATH", hint = "install trovador, or run this where it is" }
  end
  if run.code ~= 0 then error{ kind = "command_failed", message = "trovador search failed: " .. run.err } end
  local a, na = counts(mine.hits, "id")
  local b, nb = counts(json.decode(run.out).hits, "session_id")
  local diffs = {}
  for id, n in pairs(a) do
    if b[id] ~= n then diffs[#diffs + 1] = { id = id, timon = n, trovador = b[id] or "missing" } end
  end
  for id, n in pairs(b) do
    if a[id] == nil then diffs[#diffs + 1] = { id = id, timon = "missing", trovador = n } end
  end
  print(string.format("%-24s timon %5d  trovador %5d  %s", q, na, nb, #diffs == 0 and "same" or (#diffs .. " differ")))
  if #diffs > 0 then differ[q] = diffs end
end

if next(differ) then
  error{ kind = "parity", message = "timon and trovador disagree", data = differ }
end
return "every query matches"
