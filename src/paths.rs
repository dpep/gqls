//! Shared on-disk locations. gqls keeps everything cacheable under one base
//! dir (`$XDG_CACHE_HOME/gqls`, else `~/.cache/gqls`) — introspection
//! responses, parsed records, and discovered schema paths.

use std::path::PathBuf;

/// Base cache directory, or `None` without a usable `XDG_CACHE_HOME` or
/// `HOME`. Only an absolute path counts, as the XDG spec says: an empty or
/// relative one resolves against the cwd, and `--clear-cache` would then
/// sweep whatever project directory happens to be called `gqls`.
pub(crate) fn cache_dir() -> Option<PathBuf> {
    let absolute = |var| {
        std::env::var_os(var)
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
    };
    let base = absolute("XDG_CACHE_HOME").or_else(|| Some(absolute("HOME")?.join(".cache")))?;
    Some(base.join("gqls"))
}

/// Delete gqls's cache files — every kind it writes, and the embedding vectors
/// releases before 0.26 wrote — returning how many were removed. Only regular
/// files are touched and no symlink is followed, so nothing outside the cache,
/// and nothing gqls didn't write, can go.
pub(crate) fn clear_cache() -> usize {
    fn clear(dir: &std::path::Path, kinds: &[&str]) -> usize {
        // A symlinked dir is left alone, like a symlinked file.
        if !std::fs::symlink_metadata(dir).is_ok_and(|m| m.is_dir()) {
            return 0;
        }
        let Ok(rd) = std::fs::read_dir(dir) else {
            return 0;
        };
        let ours = |p: &std::path::Path| {
            p.extension()
                .and_then(|x| x.to_str())
                .is_some_and(|x| kinds.contains(&x) || x.starts_with("tmp"))
        };
        rd.flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_file()) && ours(&e.path()))
            .filter(|e| std::fs::remove_file(e.path()).is_ok())
            .count()
    }
    cache_dir().map_or(0, |d| {
        clear(&d, &["rcds", "disc", "vecs"]) + clear(&d.join("introspect"), &["json"])
    })
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
