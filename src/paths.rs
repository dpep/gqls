//! Shared on-disk locations. gqls keeps everything cacheable under one base
//! dir (`$XDG_CACHE_HOME/gqls`, else `~/.cache/gqls`) — embedding vectors, the
//! introspection response cache, and the background-warm lockfiles.

use std::path::PathBuf;

/// Base cache directory, or `None` if neither `XDG_CACHE_HOME` nor `HOME` is set.
pub fn cache_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(base.join("gqls"))
}

/// Render a path for display, shortening the home directory to `~`.
pub fn display(p: &std::path::Path) -> String {
    if let Some(home) = std::env::var_os("HOME") {
        if let Ok(rest) = p.strip_prefix(&home) {
            return format!("~/{}", rest.display());
        }
    }
    p.display().to_string()
}

/// Directory for ephemeral files (the background-warm single-flight lockfiles).
/// The system temp dir so the OS reaps them — they never litter the cache. It's
/// per-user on macOS (`$TMPDIR`) and `/tmp` on Linux, and stable within a login,
/// so concurrent gqls processes still see the same lock.
pub fn temp_dir() -> PathBuf {
    std::env::temp_dir().join("gqls")
}
