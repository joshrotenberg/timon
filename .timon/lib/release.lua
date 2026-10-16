-- This repository's release, as two tasks. main is protected, so a version change goes
-- through a pull request (bump), and the tag and GitHub release follow once it merges
-- (release). tasks.lua adds check as release's dependency.
local M = {}

local repo_opt = { type = "string?", doc = "owner/name on GitHub (default: from origin)" }
local remote_opt = { type = "string", default = "origin", doc = "the remote to push to" }

local function fail(message, hint)
  error{ kind = "release", message = message, hint = hint }
end

-- On the default branch, with nothing to commit and nothing to pull.
local function ready(opts)
  local base = gh.repo{ repo = opts.repo }.default_branch
  if git.branch() ~= base then
    fail("releases start from " .. base, "git switch " .. base)
  end
  git.run{ "fetch", "--quiet", opts.remote }
  local s = git.status()
  if not s.clean then
    fail("the working tree has changes", "commit or stash them first")
  end
  if s.behind > 0 or s.ahead > 0 then
    fail(string.format("%s is %d ahead and %d behind %s", base, s.ahead, s.behind, s.upstream),
      "pull or push first")
  end
  return base
end

M.bump = {
  description = "Open a pull request that sets the version: timon bump --version 0.2.0",
  group = "release",
  options = {
    version = { type = "string", doc = "the new version, such as 0.2.0" },
    repo = repo_opt,
    remote = remote_opt,
  },
  run = function(opts)
    if not opts.version:match("^%d+%.%d+%.%d+[%w.+-]*$") then
      fail("\"" .. opts.version .. "\" is not a version", "use MAJOR.MINOR.PATCH, such as 0.2.0")
    end
    local base = ready(opts)
    local tag = "v" .. opts.version
    local branch = "release/" .. tag
    git.run{ "switch", "--quiet", "--create", branch }
    local old = toml.edit("Cargo.toml", "package.version", opts.version)
    if fs.exists("Cargo.lock") then
      sh{ "cargo", "update", "--workspace", "--offline", "--quiet" }
    end
    git.commit{ message = "chore: release " .. tag, all = true }
    git.push{ remote = opts.remote, ref = branch, set_upstream = true }
    local pr = gh.api("repos/{owner}/{repo}/pulls", {
      method = "POST",
      repo = opts.repo,
      json = {
        title = "chore: release " .. tag,
        head = branch,
        base = base,
        body = "Sets the version to " .. opts.version .. " (from " .. old .. "). "
          .. "After it merges, run timon release on " .. base .. ".",
      },
    })
    return { from = old, to = opts.version, branch = branch, pr = pr.html_url }
  end,
}

M.release = {
  description = "Tag the default branch at Cargo.toml's version, push the tag, publish a GitHub release",
  group = "release",
  confirm = "Tag, push and publish a GitHub release?",
  options = {
    draft = { type = "boolean", doc = "publish the GitHub release as a draft" },
    repo = repo_opt,
    remote = remote_opt,
  },
  run = function(opts)
    ready(opts)
    local version = toml.decode(fs.read("Cargo.toml")).package.version
    local tag = "v" .. version
    local remote_tag = git.run{ "ls-remote", "--tags", opts.remote, "refs/tags/" .. tag }
    if tbl.find(git.tags(), function(t) return t == tag end) or remote_tag ~= "" then
      fail(tag .. " exists already", "bump the version first: timon bump --version X.Y.Z")
    end
    git.tag(tag, { message = "timon " .. version })
    git.push{ remote = opts.remote, ref = tag }
    local r = gh.api("repos/{owner}/{repo}/releases", {
      method = "POST",
      repo = opts.repo,
      json = {
        tag_name = tag,
        name = "timon " .. version,
        generate_release_notes = true,
        draft = opts.draft,
      },
    })
    return { tag = tag, url = r.html_url }
  end,
}

return M
