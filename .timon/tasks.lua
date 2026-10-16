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
}
