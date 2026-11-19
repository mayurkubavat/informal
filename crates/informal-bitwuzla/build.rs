//! Compiles the C++ shim against Bitwuzla when informal-sat found it (and links
//! it): informal-sat passes its install root as DEP_CADICAL_BITWUZLA_ROOT.

fn main() {
    println!("cargo:rerun-if-changed=cpp/shim.cpp");
    println!("cargo:rustc-check-cfg=cfg(informal_bitwuzla)");
    let Some(root) = std::env::var_os("DEP_CADICAL_BITWUZLA_ROOT") else {
        println!("cargo:warning=Bitwuzla not found; the word-level engines are left out");
        return;
    };
    let root = std::path::PathBuf::from(root);
    cc::Build::new()
        .cpp(true)
        .std("c++17")
        .file("cpp/shim.cpp")
        .include(root.join("include"))
        .opt_level(2)
        .compile("informal_bitwuzla_shim");
    // The shim needs the libraries after it on the link line, even when
    // informal-sat (which also links them) is otherwise unused.
    for d in ["lib/x86_64-linux-gnu", "lib", "lib64"] {
        let dir = root.join(d);
        if dir.join("libbitwuzla.a").is_file() {
            println!("cargo:rustc-link-search=native={}", dir.display());
        }
    }
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
    for l in [
        "gmp",
        "mpfr",
        if cfg!(target_os = "macos") {
            "c++"
        } else {
            "stdc++"
        },
    ] {
        println!("cargo:rustc-link-lib=dylib={l}");
    }
    println!("cargo:rustc-cfg=informal_bitwuzla");
}
