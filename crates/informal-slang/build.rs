//! Compiles the C++ shim against slang when it is built. Looks in
//! `INFORMAL_SLANG_DIR` (the slang checkout with its `build/`), then
//! `<tools>/slang`, where `<tools>` is `INFORMAL_TOOLS_DIR` (default
//! `~/Silicon/DV/tools`). Without it the crate still builds and
//! `available()` is false.

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=INFORMAL_SLANG_DIR");
    println!("cargo:rerun-if-env-changed=INFORMAL_TOOLS_DIR");
    println!("cargo:rerun-if-changed=cpp/shim.cpp");
    println!("cargo:rerun-if-changed=cpp/lower.cpp");
    println!("cargo:rustc-check-cfg=cfg(informal_slang)");
    let dir = std::env::var_os("INFORMAL_SLANG_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("INFORMAL_TOOLS_DIR")
                .map(PathBuf::from)
                .or_else(|| {
                    std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Silicon/DV/tools"))
                })
                .map(|t| t.join("slang"))
        });
    let Some(dir) = dir.filter(|d| d.join("build/lib/libsvlang.a").is_file()) else {
        println!("cargo:warning=slang not found (set INFORMAL_SLANG_DIR); the SystemVerilog checks are skipped");
        return;
    };
    let b = dir.join("build");
    let deps = b.join("_deps");
    cc::Build::new()
        .cpp(true)
        .std("c++20")
        .file("cpp/shim.cpp")
        .file("cpp/lower.cpp")
        .include(dir.join("include"))
        .include(b.join("source"))
        .include(dir.join("external"))
        .include(deps.join("boost_regex-src/include"))
        .include(deps.join("fmt-src/include"))
        .include(deps.join("mimalloc-src/include"))
        .define("SLANG_STATIC_DEFINE", None)
        .define("SLANG_USE_THREADS", None)
        .define("BOOST_REGEX_STANDALONE", None)
        .flag_if_supported("-Wno-unused-parameter")
        .opt_level(2)
        .compile("informal_slang_shim");
    println!(
        "cargo:rerun-if-changed={}",
        b.join("lib/libsvlang.a").display()
    );
    println!("cargo:rustc-link-search=native={}", b.join("lib").display());
    for lib in ["svlang", "fmt", "mimalloc"] {
        println!("cargo:rustc-link-lib=static={lib}");
    }
    let cxx = if cfg!(target_os = "macos") {
        "c++"
    } else {
        "stdc++"
    };
    println!("cargo:rustc-link-lib=dylib={cxx}");
    println!("cargo:rustc-cfg=informal_slang");
}
