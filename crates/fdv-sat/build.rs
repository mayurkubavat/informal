//! Links CaDiCaL's static library when it is built. Looks in
//! `FDV_CADICAL_DIR` (the CaDiCaL checkout), then ~/Silicon/DV/tools/cadical.
//! Without it the crate still builds, and `Cadical::new` returns an error.

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=FDV_CADICAL_DIR");
    println!("cargo:rustc-check-cfg=cfg(fdv_cadical)");
    let candidates: Vec<PathBuf> = std::env::var_os("FDV_CADICAL_DIR")
        .map(PathBuf::from)
        .into_iter()
        .chain(std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Silicon/DV/tools/cadical")))
        .collect();
    for dir in candidates {
        let lib = dir.join("build/libcadical.a");
        println!("cargo:rerun-if-changed={}", lib.display());
        if lib.is_file() {
            println!(
                "cargo:rustc-link-search=native={}",
                dir.join("build").display()
            );
            println!("cargo:rustc-link-lib=static=cadical");
            let cxx = if cfg!(target_os = "macos") {
                "c++"
            } else {
                "stdc++"
            };
            println!("cargo:rustc-link-lib=dylib={cxx}");
            println!("cargo:rustc-cfg=fdv_cadical");
            return;
        }
    }
    println!("cargo:warning=CaDiCaL not found (set FDV_CADICAL_DIR); the in-house engines stay out of the portfolio until it is built");
}
