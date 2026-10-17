-- Fixes groups of GitHub issues with the fixer agent, each group in its own worktree, then gates and reviews each branch. Spends tokens.
-- timon --max-usd 30 fix-issues 116 117,118 ...   one argument per group; a group shares a worktree.
local groups = {}
for _, a in ipairs(arg) do
  local g = {}
  for n in a:gmatch("%d+") do g[#g + 1] = tonumber(n) end
  if #g > 0 then groups[#groups + 1] = g end
end
if #groups == 0 then
  error{ kind = "usage", message = "give one argument per group of issues, such as 117,118" }
end

local function issue_text(n)
  local i = gh.api("repos/{owner}/{repo}/issues/" .. n)
  return string.format("## #%d %s\n\n%s", n, i.title, i.body or "")
end

local function tail(text, n)
  local lines = {}
  for l in (text or ""):gmatch("[^\n]+") do lines[#lines + 1] = l end
  return table.concat(lines, "\n", math.max(1, #lines - n + 1))
end

-- The deterministic gate: the repository's own checks, in the worktree.
local function gate(cwd)
  for _, cmd in ipairs({
    { "cargo", "fmt", "--all", "--", "--check" },
    { "cargo", "clippy", "--all-targets", "--", "-D", "warnings" },
    { "cargo", "test", "--quiet" },
  }) do
    local r = proc.run(cmd, { cwd = cwd, timeout = "40m" })
    if r.code ~= 0 then
      return { passed = false, failed = table.concat(cmd, " "), output = tail(r.out .. "\n" .. r.err, 40) }
    end
  end
  return { passed = true }
end

local runs = {}  -- the fixer's record for each group, for its worktree
local result = flow.pipeline(groups, {
  { name = "fix", run = function(group, _, i)
    local texts = {}
    for _, n in ipairs(group) do texts[#texts + 1] = issue_text(n) end
    local r = agent.run{
      agent = "fixer", worktree = true, label = "fix " .. table.concat(group, ","),
      prompt = "Fix these issues, one commit each:\n\n" .. table.concat(texts, "\n\n---\n\n"),
    }
    runs[i] = r
    return r
  end },
  { name = "gate", run = function(_, _, i)
    local wt = runs[i].worktree
    if not wt then return { passed = false, failed = "no worktree: the fixer changed nothing" } end
    return gate(wt.path)
  end },
  { name = "review", run = function(checked, group, i)
    if not checked.passed then return checked end
    return agent.run{
      agent = "reviewer", cwd = runs[i].worktree.path, label = "review " .. table.concat(group, ","),
      prompt = "Review the commits on this branch that are not on main (git log main..HEAD, git diff main...HEAD) "
        .. "against issues " .. table.concat(group, ", ") .. ". Read the issues with gh issue view if you need them.",
    }
  end },
}, { limit = #groups })

local report = {}
for i, group in ipairs(groups) do
  local r, item = runs[i], result.items[i]
  local fix = r and r.answer
  report[i] = {
    issues = group,
    fixer = r and r.status,
    branch = r and r.worktree and r.worktree.branch,
    path = r and r.worktree and r.worktree.path,
    stopped_at = item.stopped_at,
    stages = item.stages,
    fix = fix,
    review = item.value,
  }
end
return { usage = result.usage, groups = report }
