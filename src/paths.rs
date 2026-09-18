//! Shared on-disk locations. gqls keeps everything cacheable under one base
//! dir (`$XDG_CACHE_HOME/gqls`, else `~/.cache/gqls`) — introspection
//! responses, parsed records, and discovered schema paths.

use std::path::PathBuf;

/// Base cache directory, or `None` if neither `XDG_CACHE_HOME` nor `HOME` is set.
pub(crate) fn cache_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(base.join("gqls"))
}

/// Render a path for display, shortening the home directory to `~`.
pub(crate) fn display(p: &std::path::Path) -> String {
    if let Some(home) = std::env::var_os("HOME") {
        if let Ok(rest) = p.strip_prefix(&home) {
            return format!("~/{}", rest.display());
        }
    }
    p.display().to_string()
}
