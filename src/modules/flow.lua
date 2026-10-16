-- flow: workflow patterns over the runner. Loaded by flow.rs.
--
-- A pattern returns a record and never raises for an outcome. Its status says whether it ran to
-- the end (ok, failed, timeout, invalid); what it concluded is a separate field (winner,
-- accepted, kept), as a review's verdict is separate from its run's status.
local scope, try, array, check, first, fail, now = ...
local type, pack, ipairs, pairs, tostring, error, floor, min, remove, sort =
  type, table.pack, ipairs, pairs, tostring, error, math.floor, math.min, table.remove, table.sort

local flow = {}

local function want(name, value, kind, pos)
  if type(value) ~= kind then
    error(fail("bad_argument",
      ("bad argument #%d to %s: expected a %s, got a %s"):format(pos, name, kind, type(value))), 3)
  end
end

local function is_run(v)
  return type(v) == "table" and type(v.id) == "string" and type(v.status) == "string"
    and v.provider ~= nil
end

local function is_record(v)
  return type(v) == "table" and type(v.pattern) == "string" and type(v.status) == "string"
end

-- What a step gave: { status, value, error, runs, ms }.
local function outcome(v, ms)
  if is_run(v) then
    local value = v.answer
    if value == nil then value = v.text end
    return { status = v.status, value = value, error = v.error, runs = { v }, ms = v.ms or ms }
  end
  if is_record(v) then
    return { status = v.status, value = v.value, error = v.error, runs = v.runs or {}, ms = v.ms or ms }
  end
  return { status = "ok", value = v, runs = {}, ms = ms }
end

-- Calls fn and settles what it returned or raised; the second value it returned comes back
-- too. Cancellation is raised again, so a pattern inside a cancelled scope stops like any other
-- work.
local function settle(fn, ...)
  local t0 = now()
  local r = pack(try(fn, ...))
  if r[1] then return outcome(r[2], now() - t0), r[3] end
  local err = r[2]
  if type(err) == "table" and err.kind == "cancelled" then error(err, 0) end
  local status = (type(err) == "table" and err.kind == "timeout") and "timeout" or "failed"
  return { status = status, error = err, runs = {}, ms = now() - t0 }
end

-- The record: fields plus the pattern's name, every agent run in its outcomes, usage and time.
-- usage counts the runs whose cost is unknown (Codex reports none) apart from the dollars.
local function finish(pattern, t0, fields, outcomes)
  local runs, usd, unknown = array(), 0, 0
  for _, o in ipairs(outcomes) do
    for _, r in ipairs(o.runs or {}) do
      runs[#runs + 1] = r
      local cost = r.usage and r.usage.usd
      if type(cost) == "number" then usd = usd + cost else unknown = unknown + 1 end
    end
  end
  fields.pattern = pattern
  fields.runs = runs
  -- Rounded to a millionth of a dollar, so sums of float costs print as written.
  fields.usage = { runs = #runs, usd = floor(usd * 1e6 + 0.5) / 1e6, unknown_cost = unknown }
  fields.ms = now() - t0
  return fields
end

local function tail(text)
  text = (text or ""):gsub("%s+$", "")
  if #text > 400 then text = "..." .. text:sub(-400) end
  return text
end

-- What an answer concludes: true or false, { passed, reason }, a yes_no answer, a review.
local function conclude(v)
  if type(v) == "boolean" then return v end
  if type(v) == "table" then
    if type(v.passed) == "boolean" then return v.passed, v.reason end
    if type(v.answer) == "boolean" then return v.answer, v.reason end
    if v.verdict == "approve" or v.verdict == "request_changes" then
      return v.verdict == "approve", v.summary
    end
  end
  return nil
end

local function is_command(g)
  return type(g) == "string" or (type(g) == "table" and type(g[1]) == "string" and g.prompt == nil)
end

-- Runs a gate on a value. The verdict's status says whether the check ran; passed says what it
-- concluded, and is nil when it did not run.
local function gate(g, value, i)
  local t0 = now()
  if type(g) == "function" then
    local o, second = settle(g, value, i)
    if o.status ~= "ok" then
      return finish("check", t0, { status = o.status, error = o.error }, { o })
    end
    local passed, reason = conclude(o.value)
    if type(o.value) == "boolean" and reason == nil then reason = second end
    if passed == nil then
      error(fail("bad_argument",
        "a check function returns true or false, { passed, reason }, or an agent record with a yes_no or review answer"), 0)
    end
    return finish("check", t0, { status = "ok", passed = passed, reason = reason }, { o })
  elseif is_command(g) then
    local r = proc.run(g)
    if r.timed_out then
      return finish("check", t0, { status = "timeout", code = r.code, reason = tail(r.err) }, {})
    end
    local output = r.err ~= "" and r.err or r.out
    return finish("check", t0, { status = "ok", passed = r.code == 0, code = r.code, reason = tail(output) }, {})
  elseif type(g) == "table" and type(g.prompt) == "string" then
    local spec = {}
    for k, v in pairs(g) do spec[k] = v end
    if spec.output == nil then spec.output = "yes_no" end
    if value ~= nil then
      spec.prompt = spec.prompt .. "\n\n" .. (type(value) == "string" and value or json.encode(value))
    end
    local o = settle(agent.run, spec)
    if o.status ~= "ok" then
      return finish("check", t0, { status = o.status, error = o.error }, { o })
    end
    local passed, reason = conclude(o.value)
    if passed == nil then
      return finish("check", t0, {
        status = "invalid",
        error = fail("invalid", "the check's answer has no passed, answer or verdict"),
      }, { o })
    end
    return finish("check", t0, { status = "ok", passed = passed, reason = reason }, { o })
  end
  error(fail("bad_argument", "a gate is a function, a command, or an agent spec with a prompt"), 0)
end

function flow.check(g, value)
  if g == nil then error(fail("bad_argument", "flow.check needs a gate"), 2) end
  return gate(g, value)
end

local function stage_list(stages)
  local out = {}
  for i, st in ipairs(stages) do
    if type(st) == "function" then
      out[i] = { name = "stage " .. i, run = st }
    elseif type(st) == "table" and type(st.run) == "function" then
      out[i] = { name = st.name or ("stage " .. i), run = st.run }
    else
      error(fail("bad_argument", ("stage %d is a function or { name, run }"):format(i)), 3)
    end
  end
  return out
end

function flow.pipeline(items, stages, opts)
  want("flow.pipeline", items, "table", 1)
  want("flow.pipeline", stages, "table", 2)
  check("flow.pipeline", opts)
  local t0, list, all = now(), stage_list(stages), {}
  local results = par.map(items, function(item, i)
    local v, done, status, stopped = item, array(), "ok", nil
    for _, st in ipairs(list) do
      local o = settle(st.run, v, item, i)
      all[#all + 1] = o
      done[#done + 1] = { name = st.name, status = o.status, error = o.error, ms = o.ms, runs = o.runs }
      if o.status ~= "ok" then
        status, stopped, v = o.status, st.name, nil
        break
      end
      v = o.value
    end
    return { item = item, status = status, value = v, stopped_at = stopped, stages = done }
  end, { limit = opts and opts.limit })
  local status, values = "ok", array()
  for i, r in ipairs(results) do
    if r.status ~= "ok" then status = "failed" end
    if r.value == nil then values[i] = json.null else values[i] = r.value end
  end
  return finish("pipeline", t0, { status = status, items = results, values = values }, all)
end

local function default_key(v)
  if type(v) == "table" then
    if v.answer ~= nil then return tostring(v.answer) end
    return json.encode(v)
  end
  return tostring(v)
end

function flow.vote(opts)
  check("flow.vote", opts)
  local n = opts.n or 3
  local quorum = opts.quorum or floor(n / 2) + 1
  local key = opts.key or default_key
  local limit = opts.limit or n
  local t0, samples, all, tally, firsts = now(), array(), {}, {}, {}
  local winner
  scope(function(s)
    local running, started = {}, 0
    local function start()
      started = started + 1
      local i = started
      running[#running + 1] = { i = i, task = s:spawn(function() return settle(opts.run, i) end) }
    end
    for _ = 1, min(limit, n) do start() end
    while #running > 0 do
      local tasks = {}
      for j, r in ipairs(running) do tasks[j] = r.task end
      local r = remove(running, first(tasks))
      local o = r.task:await()
      all[#all + 1] = o
      local sample = { index = r.i, status = o.status, value = o.value, error = o.error }
      if o.status == "ok" then
        local k = key(o.value)
        sample.key = k
        tally[k] = (tally[k] or 0) + 1
        if firsts[k] == nil then firsts[k] = o.value end
        if tally[k] >= quorum then winner = k end
      end
      samples[#samples + 1] = sample
      if winner then
        s:cancel()
        return
      end
      if started < n then start() end
    end
  end)
  local agreed = winner ~= nil
  if not agreed then
    local best, tie = 0, false
    for k, count in pairs(tally) do
      if count > best then winner, best, tie = k, count, false
      elseif count == best then tie = true end
    end
    if tie then winner = nil end
  end
  local any_ok = next(tally) ~= nil
  return finish("vote", t0, {
    status = any_ok and "ok" or "failed",
    winner = winner,
    value = winner and firsts[winner],
    agreed = agreed,
    quorum = quorum,
    tally = tally,
    samples = samples,
  }, all)
end

function flow.best_of(opts)
  check("flow.best_of", opts)
  if (opts.score == nil) == (opts.judge == nil) then
    error(fail("bad_option", "flow.best_of takes score or judge, one of them"), 2)
  end
  local n, t0 = opts.n or 3, now()
  local indexes = {}
  for i = 1, n do indexes[i] = i end
  local outcomes = par.map(indexes, function(i) return settle(opts.run, i) end, { limit = opts.limit })
  local candidates, ok, all = array(), {}, {}
  for i, o in ipairs(outcomes) do
    all[#all + 1] = o
    candidates[i] = { index = i, status = o.status, value = o.value, error = o.error }
    if o.status == "ok" then ok[#ok + 1] = candidates[i] end
  end
  if #ok == 0 then
    return finish("best_of", t0, { status = "failed", candidates = candidates }, all)
  end
  local winner, scores
  if opts.score then
    scores = {}
    local best
    for _, c in ipairs(ok) do
      local s = opts.score(c.value, c.index)
      if type(s) ~= "number" then
        error(fail("bad_argument", "score returns a number, got a " .. type(s)), 2)
      end
      c.score = s
      scores[tostring(c.index)] = s
      if best == nil or s > best then winner, best = c.index, s end
    end
  else
    local values = {}
    for j, c in ipairs(ok) do values[j] = c.value end
    local o = settle(opts.judge, values)
    all[#all + 1] = o
    if o.status ~= "ok" then
      return finish("best_of", t0, { status = o.status, error = o.error, stopped_at = "judge", candidates = candidates }, all)
    end
    local pick = o.value
    if type(pick) == "table" then pick = pick.index or pick.pick end
    if type(pick) ~= "number" or ok[pick] == nil then
      return finish("best_of", t0, {
        status = "invalid",
        error = fail("invalid", "the judge returned no index into the candidates it was given"),
        stopped_at = "judge",
        candidates = candidates,
      }, all)
    end
    winner = ok[pick].index
  end
  return finish("best_of", t0, {
    status = "ok", winner = winner, value = candidates[winner].value, scores = scores, candidates = candidates,
  }, all)
end

function flow.refine(opts)
  check("flow.refine", opts)
  local rounds, t0 = opts.rounds or 3, now()
  local all, verdicts = {}, array()
  local o = settle(opts.draft)
  all[#all + 1] = o
  if o.status ~= "ok" then
    return finish("refine", t0, { status = o.status, error = o.error, stopped_at = "draft", accepted = false, verdicts = verdicts }, all)
  end
  local value, last, accepted = o.value, o.runs[1], false
  for round = 1, rounds do
    local v = gate(opts.check, value, round)
    verdicts[#verdicts + 1] = v
    all[#all + 1] = v
    if v.status ~= "ok" then
      return finish("refine", t0, {
        status = v.status, error = v.error, stopped_at = "check", value = value, accepted = false, verdicts = verdicts,
      }, all)
    end
    if v.passed then
      accepted = true
      break
    end
    if round == rounds then break end
    local r = settle(opts.revise, value, v, round, last)
    all[#all + 1] = r
    if r.status ~= "ok" then
      return finish("refine", t0, {
        status = r.status, error = r.error, stopped_at = "revise", value = value, accepted = false, verdicts = verdicts,
      }, all)
    end
    value, last = r.value, r.runs[1] or last
  end
  return finish("refine", t0, {
    status = "ok", value = value, accepted = accepted, rounds = #verdicts, verdicts = verdicts,
  }, all)
end

function flow.verify(opts)
  check("flow.verify", opts)
  local gates = opts.checks or (opts.check ~= nil and { opts.check }) or nil
  if gates == nil or #gates == 0 then
    error(fail("bad_option", "flow.verify needs check or checks"), 2)
  end
  local keep, t0, all = opts.keep or "majority", now(), {}
  local results = par.map(opts.items, function(item, i)
    local verdicts = par.map(gates, function(g) return gate(g, item, i) end)
    local passes, fails = 0, 0
    for _, v in ipairs(verdicts) do
      all[#all + 1] = v
      if v.status == "ok" then
        if v.passed then passes = passes + 1 else fails = fails + 1 end
      end
    end
    local kept
    if keep == "all" then kept = passes == #gates
    elseif keep == "any" then kept = passes > 0
    else kept = fails * 2 <= #gates end
    return { item = item, kept = kept, passed = passes, rejected = fails, verdicts = verdicts }
  end, { limit = opts.limit })
  local kept, rejected = array(), array()
  for _, r in ipairs(results) do
    if r.kept then kept[#kept + 1] = r.item else rejected[#rejected + 1] = r.item end
  end
  return finish("verify", t0, {
    status = "ok", value = kept, kept = kept, rejected = rejected, items = results,
  }, all)
end

function flow.retry(fn, opts)
  want("flow.retry", fn, "function", 1)
  check("flow.retry", opts)
  local times = opts and opts.times or 2
  local on = {}
  for _, s in ipairs(opts and opts.on or { "failed", "timeout", "invalid" }) do on[s] = true end
  local t0, all, last = now(), {}, nil
  local attempts = 0
  for attempt = 1, times + 1 do
    attempts = attempt
    last = settle(fn, attempt, last)
    all[#all + 1] = last
    if not on[last.status] then break end
  end
  return finish("retry", t0, {
    status = last.status, value = last.value, error = last.error, attempts = attempts,
  }, all)
end

function flow.plan(opts)
  check("flow.plan", opts)
  local steps, limit, t0 = opts.steps, opts.limit or math.huge, now()
  local names = {}
  for name in pairs(steps) do names[#names + 1] = name end
  sort(names)
  for _, name in ipairs(names) do
    for _, d in ipairs(steps[name].needs or {}) do
      if steps[d] == nil then
        error(fail("bad_option", ("step %q needs %q, which is not a step"):format(name, d)), 2)
      end
    end
  end
  local state = {}
  local function visit(name, path)
    if state[name] == "done" then return end
    if state[name] == "visiting" then
      error(fail("bad_option", "a cycle in the steps: " .. table.concat(path, " -> ") .. " -> " .. name), 3)
    end
    state[name] = "visiting"
    path[#path + 1] = name
    for _, d in ipairs(steps[name].needs or {}) do visit(d, path) end
    path[#path] = nil
    state[name] = "done"
  end
  for _, name in ipairs(names) do visit(name, {}) end

  local results, order, started, stopped, all = {}, array(), {}, nil, {}
  scope(function(s)
    local running = {}
    local function ready(name)
      for _, d in ipairs(steps[name].needs or {}) do
        if results[d] == nil or results[d].status ~= "ok" then return false end
      end
      return true
    end
    local function launch()
      if stopped then return end
      for _, name in ipairs(names) do
        if #running >= limit then return end
        if not started[name] and ready(name) then
          started[name] = true
          local deps = {}
          for _, d in ipairs(steps[name].needs or {}) do deps[d] = results[d].value end
          running[#running + 1] = {
            name = name,
            task = s:spawn(function() return settle(steps[name].run, deps) end),
          }
        end
      end
    end
    launch()
    while #running > 0 do
      local tasks = {}
      for j, r in ipairs(running) do tasks[j] = r.task end
      local r = remove(running, first(tasks))
      local o = r.task:await()
      results[r.name] = o
      all[#all + 1] = o
      order[#order + 1] = r.name
      if o.status ~= "ok" and not stopped then stopped = r.name end
      launch()
    end
  end)
  local out, skipped, values = {}, array(), {}
  for _, name in ipairs(names) do
    local o = results[name]
    if o then
      out[name] = { status = o.status, value = o.value, error = o.error, ms = o.ms, runs = o.runs }
      values[name] = o.value
    else
      skipped[#skipped + 1] = name
    end
  end
  return finish("plan", t0, {
    status = stopped and results[stopped].status or "ok",
    stopped_at = stopped,
    value = values,
    steps = out,
    order = order,
    skipped = skipped,
  }, all)
end

return flow
