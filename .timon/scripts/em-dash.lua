-- Finds em dashes in timon's own text; fails if there are any (AGENTS.md: no em dashes).
-- Usage: timon em-dash [ROOT]
local repo = require("repo")

local DASH = "\u{2014}"
local root = arg[1] or fs.cwd()
local found = {}
for _, file in ipairs(repo.text_files(root)) do
  for n, line in ipairs(str.lines(fs.read(file))) do
    if line:find(DASH, 1, true) then
      found[#found + 1] = str.fmt("${file}:${n}: ${line}", { file = file, n = n, line = str.trim(line) })
    end
  end
end
if #found > 0 then
  error({
    kind = "em_dash",
    message = #found .. " line(s) with an em dash:\n" .. table.concat(found, "\n"),
    hint = "use a colon, a comma, parentheses or two sentences",
  })
end
return #repo.text_files(root) .. " files, no em dashes"
