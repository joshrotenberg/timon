-- timon's doc site: an mdbook generated from the README, the skills, help() and the built-in
-- task declarations, so no page is written by hand (#168). tasks.lua's book task calls build.
local M = {}

local REPO_URL = "https://github.com/joshrotenberg/timon"

-- The README's sections and the pages they go to. A section missing here fails the build, so a
-- new section gets a place in the book instead of dropping out of it.
local README_PAGES = {
  Status = "README.md",
  Install = "install.md",
  Use = "use.md",
  ["Working on timon"] = "contributing.md",
  License = "README.md",
}

-- Each skill, the guide page it becomes and its title in the summary, in reading order. A skill
-- missing here fails the build too.
local GUIDE = {
  { skill = "timon", page = "guide/basics.md", title = "Basics" },
  { skill = "timon-tasks", page = "guide/tasks.md", title = "Tasks and hooks" },
  { skill = "timon-scripting", page = "guide/scripting.md", title = "Scripting" },
  { skill = "timon-agents", page = "guide/agents.md", title = "Agents" },
  { skill = "timon-workflows", page = "guide/workflows.md", title = "Workflows" },
  { skill = "timon-sessions", page = "guide/sessions.md", title = "Sessions" },
}

-- Files that become pages with only their links changed.
local DOCS = {
  { file = "docs/design.md", page = "design.md", title = "Design" },
  { file = "docs/prior-art.md", page = "prior-art.md", title = "Prior art" },
  { file = "docs/live-tests.md", page = "live-tests.md", title = "Live tests" },
}

local function fail(message, hint)
  error{ kind = "book", message = message, hint = hint }
end

local function lines_of(text)
  local lines = {}
  for line in (text:gsub("\n$", "") .. "\n"):gmatch("(.-)\n") do lines[#lines + 1] = line end
  return lines
end

-- a/./b/../c becomes a/c.
local function normalize(p)
  local parts = {}
  for part in p:gmatch("[^/]+") do
    if part == ".." and #parts > 0 and parts[#parts] ~= ".." then
      table.remove(parts)
    elseif part ~= "." then
      parts[#parts + 1] = part
    end
  end
  return table.concat(parts, "/")
end

local function dir_of(p)
  return p:match("^(.*)/[^/]*$") or ""
end

-- The link from page `from` to page `to`, both relative to the book's src directory.
local function relative(from, to)
  local a, b = {}, {}
  for part in dir_of(from):gmatch("[^/]+") do a[#a + 1] = part end
  for part in to:gmatch("[^/]+") do b[#b + 1] = part end
  local i = 1
  while i <= #a and i < #b and a[i] == b[i] do i = i + 1 end
  local out = {}
  for _ = i, #a do out[#out + 1] = ".." end
  for j = i, #b do out[#out + 1] = b[j] end
  return table.concat(out, "/")
end

-- A heading's anchor, the way GitHub and mdbook make it.
local function slug(heading)
  return (heading:lower():gsub("[^%w%s%-]", ""):gsub("%s", "-"))
end

-- Prose in help text, made safe for Markdown: a * is not emphasis and <word is not a tag.
local function escape(text)
  local s = text:gsub("%*", "\\*")
  s = s:gsub("<(%a)", "&lt;%1")
  return s
end

-- Rewrites a page line by line, leaving code alone. prose(text) rewrites text outside code;
-- code(span, before) returns an inline code span with its backticks, given the text before it.
-- A fence's info string loses " fragment", which only tests/skills.rs reads.
local function transform(text, prose, code)
  local out, fence = {}, nil
  for _, line in ipairs(lines_of(text)) do
    local ticks = line:match("^%s*(```+)") or line:match("^%s*(~~~+)")
    if fence then
      if ticks and ticks:sub(1, 1) == fence:sub(1, 1) and #ticks >= #fence
        and line:match("^%s*[`~]+%s*$") then
        fence = nil
      end
      out[#out + 1] = line
    elseif ticks then
      fence = ticks
      out[#out + 1] = (line:gsub("^(%s*[`~]+%s*[%w-]+) fragment%s*$", "%1"))
    else
      local pieces, start = {}, 1
      while true do
        local i = line:find("`", start, true)
        if not i then
          pieces[#pieces + 1] = line:sub(start)
          break
        end
        pieces[#pieces + 1] = line:sub(start, i - 1)
        start = i + 1
      end
      local parts = { prose(pieces[1]) }
      for k = 2, #pieces do
        if k % 2 == 1 then
          parts[#parts + 1] = prose(pieces[k])
        elseif k < #pieces then
          parts[#parts + 1] = code(pieces[k], parts[#parts])
        else
          parts[#parts + 1] = "`" .. pieces[k]  -- an unclosed backtick
        end
      end
      out[#out + 1] = table.concat(parts)
    end
  end
  return table.concat(out, "\n") .. "\n"
end

local function plain_code(span) return "`" .. span .. "`" end

-- A page from a repository file: relative links go to the page that holds their target, or to
-- the file on GitHub, and `timon skill show NAME` links to that skill's guide page. A link to a
-- file the repository does not have is added to book.broken.
local function rewrite(text, source, page, book)
  local src_dir = dir_of(source)
  local function target(t)
    if t:match("^%a[%w+.-]*:") or t:match("^#") then return t end
    local file, anchor = t:match("^([^#]*)(.*)$")
    local repo_path = normalize(src_dir == "" and file or (src_dir .. "/" .. file))
    local to = book.sources[repo_path]
    if repo_path == "README.md" and book.readme_anchors[anchor] then
      to, anchor = book.readme_anchors[anchor], ""
    end
    if to then return relative(page, to) .. anchor end
    if not fs.exists(path.join(book.root, repo_path)) then
      book.broken[#book.broken + 1] = source .. " -> " .. t
    end
    return REPO_URL .. "/blob/main/" .. repo_path .. anchor
  end
  local function prose(s)
    return (s:gsub("(%]%()([^)%s]+)(%))", function(open, t, close) return open .. target(t) .. close end))
  end
  local function code(span, before)
    local name = span:match("^timon skill show ([%w-]+)$")
    local to = name and book.skill_pages[name]
    if to and not before:match("%[$") then
      return "[`" .. span .. "`](" .. relative(page, to) .. ")"
    end
    return plain_code(span)
  end
  return transform(text, prose, code)
end

-- Relative links on a page whose target is not a page of the book.
local function broken_links(page, text, pages)
  local broken = {}
  transform(text, function(s)
    for t in s:gmatch("%]%(([^)%s]+)%)") do
      local file = t:match("^([^#]*)")
      if not t:match("^%a[%w+.-]*:") and file ~= "" then
        local to = normalize(dir_of(page) == "" and file or (dir_of(page) .. "/" .. file))
        if not pages[to] then broken[#broken + 1] = page .. " -> " .. t end
      end
    end
    return s
  end, plain_code)
  return broken
end

local function generated(source)
  return "<!-- Generated by timon book from " .. source .. ". Edit that, not this page. -->\n\n"
end

-- One function of a module: its signature, description, options and example from
-- help(name), or the module's one-line description when the function has no help of its own.
local function function_section(sig, short)
  local name = sig:match("^([%a_][%w_]*%.[%a_][%w_]*)") or sig:match("^([%a_][%w_]*)[%({]")
  local ok, text = false, nil
  if name then ok, text = try(help, name) end
  local out = { "## `" .. sig .. "`", "" }
  if not ok or type(text) ~= "string" or not text:find("\n") then
    if short ~= "" then
      out[#out + 1] = escape(short)
      out[#out + 1] = ""
    end
    return out
  end
  local section, desc, options, example = "desc", {}, {}, {}
  local lines = lines_of(text)
  for i = 2, #lines do
    local line = lines[i]
    if line == "Options:" then
      section = "options"
    elseif line == "Example:" then
      section = "example"
    elseif section == "options" then
      local opt, ty = line:match("^  (%S+)%s+(.-)%s*$")
      if opt then
        options[#options + 1] = { name = opt, type = ty, doc = {} }
      elseif line:match("%S") and #options > 0 then
        table.insert(options[#options].doc, (line:gsub("^%s+", "")))
      end
    elseif section == "example" then
      example[#example + 1] = (line:gsub("^  ", ""))
    else
      desc[#desc + 1] = line
    end
  end
  local para = {}
  local function flush()
    if #para > 0 then
      out[#out + 1] = escape(table.concat(para, " "))
      out[#out + 1] = ""
      para = {}
    end
  end
  for _, line in ipairs(desc) do
    if line:match("%S") then para[#para + 1] = line:gsub("^%s+", "") else flush() end
  end
  flush()
  for _, o in ipairs(options) do
    local doc = table.concat(o.doc, " ")
    out[#out + 1] = "- `" .. o.name .. "` (`" .. o.type .. "`)" .. (doc ~= "" and (": " .. escape(doc)) or "")
  end
  if #options > 0 then out[#out + 1] = "" end
  while #example > 0 and not example[#example]:match("%S") do table.remove(example) end
  if #example > 0 then
    out[#out + 1] = "```lua"
    for _, line in ipairs(example) do out[#out + 1] = line end
    out[#out + 1] = "```"
    out[#out + 1] = ""
  end
  return out
end

-- A module's page from help(name): the module's description, then each function.
local function module_page(name)
  local lines = lines_of(help(name))
  local short = lines[1]
  if short:sub(1, #name + 1) == name .. ":" then short = short:sub(#name + 2):gsub("^%s+", "") end
  local entries = {}
  for i = 2, #lines do
    local line = lines[i]
    if line:match("^  %S") then
      entries[#entries + 1] = { sig = line:gsub("^%s+", ""):gsub("%s+$", ""), short = "" }
    elseif line:match("^%s+%S") and #entries > 0 then
      local e = entries[#entries]
      e.short = (e.short .. " " .. line:gsub("^%s+", "")):gsub("^ ", "")
    end
  end
  local out = { generated("help(\"" .. name .. "\")") .. "# " .. name, "", escape(short), "" }
  for _, e in ipairs(entries) do
    for _, line in ipairs(function_section(e.sig, e.short)) do out[#out + 1] = line end
  end
  return table.concat(out, "\n")
end

-- The modules help() lists, as { name, description }.
local function modules()
  local list = {}
  for _, line in ipairs(lines_of(help())) do
    local name, desc = line:match("^  ([%w_]+)%s+(.+)$")
    if name then list[#list + 1] = { name = name, description = desc } end
  end
  if #list == 0 then fail("help() listed no modules") end
  return list
end

-- timon's own commands and their flags, from `bin tasks --all --json`.
local function commands_page(bin, root)
  local ok, r = try(proc.run, { bin, "tasks", "--all", "--json", cwd = root })
  if not ok then fail("cannot run " .. bin .. ": " .. r.message, "pass --bin with the timon to describe") end
  if r.code ~= 0 then fail(bin .. " tasks --all --json exited " .. r.code .. ": " .. r.err) end
  local out = {
    generated("timon tasks --all --json") .. "# Commands",
    "",
    "timon's own commands, declared the way a project's tasks are. Global flags go before the command; a command's flags go after its name. `timon NAME --help` prints the same.",
    "",
  }
  local function present(v) return v ~= nil and v ~= json.null end
  -- One argument or flag: `--limit` (`integer`, default `20`): Most runs to list.
  local function item(name, o)
    local meta = { "`" .. o.type .. "`" }
    if present(o.default) then meta[#meta + 1] = "default `" .. tostring(o.default) .. "`" end
    if o.required then meta[#meta + 1] = "required" end
    local doc = present(o.doc) and o.doc ~= "" and (": " .. escape(o.doc)) or ""
    return "- `" .. name .. "` (" .. table.concat(meta, ", ") .. ")" .. doc
  end
  for _, t in ipairs(json.decode(r.out)) do
    if t.layer == "builtin" then
      local usage = { "timon", t.name ~= "timon" and t.name or nil }
      for _, p in ipairs(t.positionals or {}) do
        local word = p.name:upper() .. (p.type:match("%[%]$") and "..." or "")
        usage[#usage + 1] = p.required and word or ("[" .. word .. "]")
      end
      out[#out + 1] = "## " .. (t.name == "timon" and "Global flags" or ("`" .. table.concat(usage, " ") .. "`"))
      out[#out + 1] = ""
      if present(t.description) and t.description ~= "" then
        out[#out + 1] = escape(t.description)
        out[#out + 1] = ""
      end
      for _, p in ipairs(t.positionals or {}) do out[#out + 1] = item(p.name:upper(), p) end
      for _, o in ipairs(t.options or {}) do
        out[#out + 1] = item((present(o.short) and (o.short .. ", ") or "") .. o.flag, o)
      end
      if #(t.positionals or {}) + #(t.options or {}) > 0 then out[#out + 1] = "" end
    end
  end
  return table.concat(out, "\n")
end

local function readme_parts(text)
  local parts, fence = { { lines = {} } }, false
  for _, line in ipairs(lines_of(text)) do
    if line:match("^%s*```") then fence = not fence end
    local heading = not fence and line:match("^## (.-)%s*$")
    if heading then
      parts[#parts + 1] = { heading = heading, lines = { line } }
    else
      table.insert(parts[#parts].lines, line)
    end
  end
  return parts
end

local BOOK_TOML = [[
[book]
title = "timon"
authors = ["Josh Rotenberg"]
description = "A task runner in one self-contained binary: tasks and scripts in Lua or Fennel, with Claude and Codex agents built in."
language = "en"
src = "src"

[build]
build-dir = "html"
create-missing = false

[output.html]
git-repository-url = "https://github.com/joshrotenberg/timon"
site-url = "/timon/"
]]

-- Writes the book's sources under opts.out (book.toml and src/), checks their links, and builds
-- the site into opts.out/html with mdbook unless opts.build is false.
function M.build(opts)
  local root = opts.root
  local out = opts.out:sub(1, 1) == "/" and opts.out or path.join(root, opts.out)
  local pages, order = {}, {}
  local book = { root = root, broken = {}, sources = {}, readme_anchors = {}, skill_pages = {} }

  -- Where each source lands, so links between them can be rewritten.
  book.sources["README.md"] = "README.md"
  for heading, page in pairs(README_PAGES) do
    if page ~= "README.md" then book.readme_anchors["#" .. slug(heading)] = page end
  end
  local skills = {}
  for _, g in ipairs(GUIDE) do
    book.sources["skills/" .. g.skill .. "/SKILL.md"] = g.page
    book.skill_pages[g.skill] = g.page
    skills[g.skill] = true
  end
  for _, dir in ipairs(fs.ls(path.join(root, "skills"))) do
    local name = path.base(dir)
    if fs.exists(path.join(root, "skills", name, "SKILL.md")) and not skills[name] then
      fail("skill " .. name .. " has no page in the book", "add it to GUIDE in .timon/lib/book.lua")
    end
  end
  for _, d in ipairs(DOCS) do book.sources[d.file] = d.page end

  local function add(page, text)
    pages[page] = text
    order[#order + 1] = page
  end

  -- The README, split into its pages; the introduction keeps the opening, Status and License.
  local intro = {}
  local readme_page = {}
  for _, part in ipairs(readme_parts(fs.read(path.join(root, "README.md")))) do
    if not part.heading then
      for _, line in ipairs(part.lines) do intro[#intro + 1] = line end
    else
      local page = README_PAGES[part.heading]
      if not page then
        fail("README section \"" .. part.heading .. "\" has no page in the book",
          "add it to README_PAGES in .timon/lib/book.lua")
      elseif page == "README.md" then
        for _, line in ipairs(part.lines) do intro[#intro + 1] = line end
      else
        part.lines[1] = "# " .. part.heading
        readme_page[page] = table.concat(part.lines, "\n")
      end
    end
  end
  add("README.md", generated("README.md") .. rewrite(table.concat(intro, "\n"), "README.md", "README.md", book))
  for _, page in ipairs({ "install.md", "use.md", "contributing.md" }) do
    if not readme_page[page] then
      fail("the README has no section for " .. page, "README_PAGES in .timon/lib/book.lua names them")
    end
    add(page, generated("README.md") .. rewrite(readme_page[page], "README.md", page, book))
  end

  for _, g in ipairs(GUIDE) do
    local source = "skills/" .. g.skill .. "/SKILL.md"
    local text = fs.read(path.join(root, source)):gsub("^%-%-%-\n.-\n%-%-%-\n+", "")
    add(g.page, generated(source) .. rewrite(text, source, g.page, book))
  end

  for _, d in ipairs(DOCS) do
    add(d.page, generated(d.file) .. rewrite(fs.read(path.join(root, d.file)), d.file, d.page, book))
  end

  add("reference/commands.md", commands_page(opts.bin, root))
  local mods = modules()
  local index = { generated("help()") .. "# Modules", "",
    "Every module a task or script can use, from `help()`. `help(name)` prints a module and `help(\"name.fn\")` one function, with its options and an example.", "" }
  for _, m in ipairs(mods) do
    index[#index + 1] = "- [" .. m.name .. "](" .. m.name .. ".md): " .. escape(m.description)
    add("reference/" .. m.name .. ".md", module_page(m.name))
  end
  add("reference/modules.md", table.concat(index, "\n") .. "\n")

  local summary = {
    generated(".timon/lib/book.lua") .. "# Summary", "",
    "[Introduction](README.md)", "",
    "- [Install](install.md)",
    "- [Use](use.md)", "",
    "# Guide", "",
  }
  for _, g in ipairs(GUIDE) do summary[#summary + 1] = "- [" .. g.title .. "](" .. g.page .. ")" end
  summary[#summary + 1] = ""
  summary[#summary + 1] = "# Reference"
  summary[#summary + 1] = ""
  summary[#summary + 1] = "- [Commands](reference/commands.md)"
  summary[#summary + 1] = "- [Modules](reference/modules.md)"
  for _, m in ipairs(mods) do
    summary[#summary + 1] = "  - [" .. m.name .. "](reference/" .. m.name .. ".md)"
  end
  summary[#summary + 1] = ""
  summary[#summary + 1] = "# Design"
  summary[#summary + 1] = ""
  summary[#summary + 1] = "- [Design](design.md)"
  summary[#summary + 1] = "- [Prior art](prior-art.md)"
  summary[#summary + 1] = ""
  summary[#summary + 1] = "# Working on timon"
  summary[#summary + 1] = ""
  summary[#summary + 1] = "- [Working on timon](contributing.md)"
  summary[#summary + 1] = "- [Live tests](live-tests.md)"
  add("SUMMARY.md", table.concat(summary, "\n") .. "\n")

  local broken = book.broken
  for _, page in ipairs(order) do
    for _, b in ipairs(broken_links(page, pages[page], pages)) do broken[#broken + 1] = b end
  end
  if #broken > 0 then
    fail("links to files that do not exist:\n" .. table.concat(broken, "\n"))
  end

  fs.write(path.join(out, "book.toml"), BOOK_TOML, { mkdir = true })
  for _, page in ipairs(order) do
    fs.write(path.join(out, "src", page), pages[page], { mkdir = true })
  end
  local wrote = #order .. " pages in " .. path.join(out, "src")
  if not opts.build then return wrote end

  local ok, err = try(sh.run, { "mdbook", "build", out })
  if not ok then
    if err.kind == "not_found" then
      fail("mdbook is not installed", "cargo install mdbook (or brew install mdbook); --no-build writes only the sources")
    end
    error(err)
  end
  return wrote .. "; the site is " .. path.join(out, "html", "index.html")
end

return M
