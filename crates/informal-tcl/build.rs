//! Links the system Tcl 8.6 library (Ubuntu: `tcl8.6-dev`; macOS: Homebrew
//! `tcl-tk@8`). Override with `INFORMAL_TCL_LIB_DIR` and `INFORMAL_TCL_LIB`.

fn main() {
    println!("cargo:rerun-if-env-changed=INFORMAL_TCL_LIB_DIR");
    println!("cargo:rerun-if-env-changed=INFORMAL_TCL_LIB");
    if let Ok(dir) = std::env::var("INFORMAL_TCL_LIB_DIR") {
        println!("cargo:rustc-link-search=native={dir}");
    } else if cfg!(target_os = "macos") {
        for dir in [
            "/opt/homebrew/opt/tcl-tk@8/lib",
            "/usr/local/opt/tcl-tk@8/lib",
        ] {
            println!("cargo:rustc-link-search=native={dir}");
        }
    }
    let lib = std::env::var("INFORMAL_TCL_LIB").unwrap_or_else(|_| "tcl8.6".into());
    println!("cargo:rustc-link-lib=dylib={lib}");
}
