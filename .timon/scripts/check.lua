-- The gates: format, lint, tests, docs and the plugin check; exits with the first failure's code.
-- Usage: timon run check [--quick], where --quick skips the tests
local quick = arg[1] == "--quick"
local gates = {
  { "format", "cargo fmt --all -- --check" },
  { "lint", "cargo clippy --all-targets -- -D warnings" },
  { "test", "cargo test", skip = quick },
  { "docs", "cargo doc --no-deps", env = { RUSTDOCFLAGS = "-D warnings" } },
  { "plugin", "cargo run -q -- plugin build --check" },
}
for _, gate in ipairs(gates) do
  local name, cmd = gate[1], gate[2]
  if gate.skip then
    log.info("skip " .. name)
  else
    log.info("gate " .. name .. ": " .. cmd)
    sh(cmd, { env = gate.env })
  end
end
return "all gates pass"
