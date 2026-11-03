//! Links CaDiCaL's static library when it is built: `FDV_CADICAL_DIR` (a
//! checkout), else the patched copy Bitwuzla builds under
//! ~/Silicon/DV/tools/bitwuzla, else ~/Silicon/DV/tools/cadical.
//! Without it the crate still builds, and `Cadical::new` returns an error.

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=FDV_CADICAL_DIR");
    println!("cargo:rustc-check-cfg=cfg(fdv_cadical)");
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    // Library directories, in order of preference: an explicit checkout,
    // then the patched CaDiCaL inside the Bitwuzla build (so a binary with
    // both carries one copy), then the plain checkout.
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(d) = std::env::var_os("FDV_CADICAL_DIR") {
        dirs.push(PathBuf::from(d).join("build"));
    }
    let sub = home.join("Silicon/DV/tools/bitwuzla/build/subprojects");
    if let Ok(rd) = std::fs::read_dir(&sub) {
        let mut found: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("cadical")))
            .map(|p| p.join("src"))
            .collect();
        found.sort();
        dirs.extend(found);
    }
    dirs.push(home.join("Silicon/DV/tools/cadical/build"));
    for dir in dirs {
        let lib = dir.join("libcadical.a");
        println!("cargo:rerun-if-changed={}", lib.display());
        if lib.is_file() {
            println!("cargo:rustc-link-search=native={}", dir.display());
            println!("cargo:rustc-link-lib=static=cadical");
            let cxx = if cfg!(target_os = "macos") { "c++" } else { "stdc++" };
            println!("cargo:rustc-link-lib=dylib={cxx}");
            println!("cargo:rustc-cfg=fdv_cadical");
            println!("cargo:warning=CaDiCaL: {}", lib.display());
            return;
        }
    }
    println!("cargo:warning=CaDiCaL not found (set FDV_CADICAL_DIR); the in-house engines stay out of the portfolio until it is built");
}
