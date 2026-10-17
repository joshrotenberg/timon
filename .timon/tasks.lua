-- This repository's tasks. The scripts in scripts/ are tasks too; timon tasks lists them all.
local release = require("release")
release.release.depends = { "check" }

return {
  bump = release.bump,
  release = release.release,
  format = {
    description = "Check formatting",
    group = "gates",
    run = "cargo fmt --all -- --check",
  },
  lint = {
    description = "Clippy, with warnings as errors",
    group = "gates",
    run = "cargo clippy --all-targets -- -D warnings",
  },
  test = {
    description = "The test suite; words after -- go to cargo test",
    group = "gates",
    run = "cargo test",
  },
  docs = {
    description = "Rustdoc, with warnings as errors",
    group = "gates",
    run = function()
      sh("cargo doc --no-deps", { env = { RUSTDOCFLAGS = "-D warnings" } })
    end,
  },
  ["plugin-check"] = {
    description = "The plugin directory matches what the binary builds",
    group = "gates",
    run = "cargo run -q -- plugin build --check",
  },
  check = {
    description = "Every gate, before a push: format, lint, test, docs, plugin-check",
    depends = { "format", "lint", "test", "docs", "plugin-check" },
    run = function() return "all gates pass" end,
  },
  quick = {
    description = "Format and lint, without the tests",
    depends = { "format", "lint" },
    run = function() return "format and lint pass" end,
  },
  update = {
    description = "Install timon and update its plugin in Claude Code and Codex",
    group = "release",
    options = {
      from = { type = '"git" | "checkout"', default = "git", doc = "install from GitHub main, or from this checkout" },
    },
    run = function(opts)
      -- Codex trusts plugin hooks by their hash, so a changed hooks.json needs /hooks again.
      local codex_home = os.getenv("CODEX_HOME") or path.join(fs.home(), ".codex")
      local codex_hooks = path.join(codex_home, "plugins/cache/timon/timon/local/hooks/hooks.json")
      local function hooks() return fs.exists(codex_hooks) and fs.read(codex_hooks) or nil end
      local before = hooks()
      -- What cargo has installed, such as "timon v0.1.0 (https://github.com/joshrotenberg/timon#ac09d4dd)".
      local function installed()
        return proc.run{ "cargo", "install", "--list" }.out:match("(timon v[^\n]*%)):") or "none"
      end
      local was = installed()

      if opts.from == "checkout" then
        sh{ "cargo", "install", "--locked", "--path", "." }
      else
        sh{ "cargo", "install", "--locked", "--git", "https://github.com/joshrotenberg/timon" }
      end
      local now = installed()
      local lines = { now == was and ("timon: already installed, " .. now) or ("timon: installed " .. now .. ", was " .. was) }

      -- Each host's steps, in order; a host that is not installed is skipped.
      for _, host in ipairs({
        { name = "claude", steps = {
          { "claude", "plugin", "marketplace", "update", "timon" },
          { "claude", "plugin", "update", "timon@timon" },
        } },
        { name = "codex", steps = {
          { "codex", "plugin", "marketplace", "upgrade", "timon" },
          { "codex", "plugin", "add", "timon@timon" },
        } },
      }) do
        for _, cmd in ipairs(host.steps) do
          local text = table.concat(cmd, " ")
          local ok, r = try(proc.run, cmd)
          if not ok and r.kind == "not_found" then
            lines[#lines + 1] = host.name .. ": not installed, skipped"
            break
          elseif not ok then
            lines[#lines + 1] = text .. ": " .. r.message
            break
          elseif r.code ~= 0 then
            local last = (r.err .. "\n" .. r.out):match("([^\n]+)%s*$") or ""
            lines[#lines + 1] = text .. ": exit " .. r.code .. ": " .. last
            break
          end
          lines[#lines + 1] = text .. ": ok"
        end
      end

      local after = hooks()
      if before and after and before ~= after then
        lines[#lines + 1] = "codex: hooks.json changed; run /hooks in Codex and trust the timon hooks again"
      end
      lines[#lines + 1] = "Sessions started before this keep the old skills and hooks."
      return table.concat(lines, "\n")
    end,
  },
}
