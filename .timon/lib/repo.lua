-- The repository's own files, for timon's dogfood scripts.
local M = {}

-- Text timon's authors write: everything but vendored code and build output.
M.text_patterns = {
  "*.md", "*.toml", "build.rs",
  ".github/**/*.yml",
  ".timon/**/*.lua", ".timon/**/*.fnl", ".timon/**/*.md",
  "contracts/*.lua",
  "docs/**/*.md",
  "skills/**/*.md",
  "src/**/*.rs", "src/**/*.lua",
  "tests/**/*.rs",
}

-- The files matching text_patterns under root (default: the working directory), sorted.
function M.text_files(root)
  root = root or fs.cwd()
  local seen, files = {}, {}
  for _, pattern in ipairs(M.text_patterns) do
    for _, file in ipairs(fs.glob(path.join(root, pattern))) do
      if not seen[file] then
        seen[file] = true
        files[#files + 1] = file
      end
    end
  end
  table.sort(files)
  return files
end

return M
