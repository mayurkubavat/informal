//! Where a desktop app serves its session: a Unix socket per app in a
//! directory only the user can enter. `informal mcp --connect` finds the
//! newest one there when it is given no path.

use std::path::PathBuf;

/// The directory of the sockets: `informal` in the user's runtime
/// directory, or a per-user directory under the temporary directory.
pub fn dir() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(d) if !d.is_empty() => PathBuf::from(d).join("informal"),
        _ => std::env::temp_dir().join(format!(
            "informal-{}",
            std::env::var("USER").unwrap_or_else(|_| "user".into())
        )),
    }
}

/// The socket of the app with this process id.
pub fn for_app(pid: u32) -> PathBuf {
    dir().join(format!("app-{pid}.sock"))
}

/// The most recently started app's socket, if any app is running.
pub fn newest() -> Option<PathBuf> {
    let mut found: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(dir())
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "sock"))
        .filter_map(|p| Some((p.metadata().ok()?.modified().ok()?, p)))
        .collect();
    found.sort();
    found.pop().map(|(_, p)| p)
}
