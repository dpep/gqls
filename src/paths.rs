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

/// Delete every file under the cache dir, including ones an older release
/// wrote and this one no longer knows; returns how many were removed. The dir
/// is gqls's alone, so nothing in it is someone else's.
pub(crate) fn clear_cache() -> usize {
    fn clear(dir: &std::path::Path) -> usize {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return 0;
        };
        rd.flatten()
            .map(|e| match e.path() {
                p if p.is_dir() => clear(&p),
                p => usize::from(std::fs::remove_file(p).is_ok()),
            })
            .sum()
    }
    cache_dir().map_or(0, |d| clear(&d))
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
