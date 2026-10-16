//! Rebuilds when an embedded directory changes. `include_dir!` does not notice a file added to
//! a directory it embeds, so a new skill or contract would otherwise be missing from the binary.

fn main() {
    for dir in ["skills", "contracts", "vendor"] {
        println!("cargo:rerun-if-changed={dir}");
    }
}
