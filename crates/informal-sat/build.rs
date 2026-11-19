//! Links CaDiCaL's static library when it is built: `INFORMAL_CADICAL_DIR` (a
//! checkout), else the patched copy Bitwuzla builds under
//! `<tools>/bitwuzla`, else `<tools>/cadical`, where `<tools>` is
//! `INFORMAL_TOOLS_DIR` (default `~/Silicon/DV/tools`).
//! When Bitwuzla is built (`<tools>/bitwuzla/install` or
//! `INFORMAL_BITWUZLA_DIR`), its static library is linked instead: it carries
//! the same patched CaDiCaL, so the binary has one copy.
//! Without any of them the crate still builds, and `Cadical::new` returns an error.

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=INFORMAL_CADICAL_DIR");
    println!("cargo:rustc-check-cfg=cfg(informal_cadical)");
    println!("cargo:rerun-if-env-changed=INFORMAL_TOOLS_DIR");
    let tools = std::env::var_os("INFORMAL_TOOLS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let local = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tools");
            if local.is_dir() {
                local
            } else {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_default()
                    .join("Silicon/DV/tools")
            }
        });
    // Bitwuzla's static library already contains its patched CaDiCaL,
    // C API included: link it instead of a second CaDiCaL, and tell crates
    // that wrap Bitwuzla (DEP_CADICAL_BITWUZLA_ROOT) where it is.
    println!("cargo:rerun-if-env-changed=INFORMAL_BITWUZLA_DIR");
    let bw = std::env::var_os("INFORMAL_BITWUZLA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| tools.join("bitwuzla/install"));
    let bwlib = ["lib/x86_64-linux-gnu", "lib", "lib64"]
        .iter()
        .map(|d| bw.join(d))
        .find(|d| d.join("libbitwuzla.a").is_file());
    if std::env::var_os("INFORMAL_CADICAL_DIR").is_none() {
        if let Some(dir) = bwlib {
            println!(
                "cargo:rerun-if-changed={}",
                dir.join("libbitwuzla.a").display()
            );
            println!("cargo:rustc-link-search=native={}", dir.display());
            for l in ["bitwuzla", "bitwuzlals", "bitwuzlabv", "bitwuzlabb"] {
                println!("cargo:rustc-link-lib=static={l}");
            }
            if cfg!(target_os = "macos") {
                for d in ["/opt/homebrew/lib", "/usr/local/lib"] {
                    if std::path::Path::new(d).is_dir() {
                        println!("cargo:rustc-link-search=native={d}");
                    }
                }
            }
            for l in ["gmp", "mpfr"] {
                println!("cargo:rustc-link-lib=dylib={l}");
            }
            let cxx = if cfg!(target_os = "macos") {
                "c++"
            } else {
                "stdc++"
            };
            println!("cargo:rustc-link-lib=dylib={cxx}");
            println!("cargo:rustc-cfg=informal_cadical");
            println!("cargo:bitwuzla_root={}", bw.display());
            return;
        }
    }
    // Library directories, in order of preference: an explicit checkout,
    // then the patched CaDiCaL inside the Bitwuzla build (so a binary with
    // both carries one copy), then the plain checkout.
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(d) = std::env::var_os("INFORMAL_CADICAL_DIR") {
        dirs.push(PathBuf::from(d).join("build"));
    }
    let sub = tools.join("bitwuzla/build/subprojects");
    if let Ok(rd) = std::fs::read_dir(&sub) {
        let mut found: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("cadical"))
            })
            .map(|p| p.join("src"))
            .collect();
        found.sort();
        dirs.extend(found);
    }
    dirs.push(tools.join("cadical/build"));
    for dir in dirs {
        let lib = dir.join("libcadical.a");
        println!("cargo:rerun-if-changed={}", lib.display());
        if lib.is_file() {
            println!("cargo:rustc-link-search=native={}", dir.display());
            println!("cargo:rustc-link-lib=static=cadical");
            let cxx = if cfg!(target_os = "macos") {
                "c++"
            } else {
                "stdc++"
            };
            println!("cargo:rustc-link-lib=dylib={cxx}");
            println!("cargo:rustc-cfg=informal_cadical");
            println!("cargo:warning=CaDiCaL: {}", lib.display());
            return;
        }
    }
    println!("cargo:warning=CaDiCaL not found (set INFORMAL_TOOLS_DIR or INFORMAL_CADICAL_DIR); the in-house engines stay out of the portfolio until it is built");
}
