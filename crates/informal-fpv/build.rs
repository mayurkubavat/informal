//! Writes one `#[test]` per design in tests/sv for tests/sv_semantics.rs, so
//! each design passes, fails and is reported on its own. A new `.sv` file
//! there is picked up on the next build.

use std::path::Path;

fn main() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/sv");
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "sv"))
        .map(|p| p.file_stem().unwrap().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let tests: String = names
        .iter()
        .map(|n| format!("#[test]\nfn sv_{n}() {{\n    case(\"{n}\");\n}}\n"))
        .collect();
    let out = std::env::var("OUT_DIR").unwrap();
    std::fs::write(Path::new(&out).join("sv_cases.rs"), tests).unwrap();
}
