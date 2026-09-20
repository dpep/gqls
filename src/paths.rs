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
/// files of those kinds go, and a symlinked *file* is never followed, so
/// nothing gqls didn't write can be deleted. A symlinked cache *directory* is
/// followed, because that's where the writes went; `introspect/` is not.
pub(crate) fn clear_cache() -> usize {
    fn clear(dir: &std::path::Path, kinds: &[&str]) -> usize {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return 0;
        };
        // `<name>.tmp<pid>` is what an interrupted write leaves. Anything
        // starting with `tmp` also matched `.tmpl` templates.
        let ours = |p: &std::path::Path| {
            p.extension().and_then(|x| x.to_str()).is_some_and(|x| {
                kinds.contains(&x)
                    || x.strip_prefix("tmp").is_some_and(|pid| {
                        !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit())
                    })
            })
        };
        rd.flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_file()) && ours(&e.path()))
            .filter(|e| std::fs::remove_file(e.path()).is_ok())
            .count()
    }
    cache_dir().map_or(0, |d| {
        // The cache dir itself is followed when it's a symlink, since that's
        // where the writes went; a symlink *inside* it is not ours to chase.
        let introspect = d.join("introspect");
        let nested = std::fs::symlink_metadata(&introspect).is_ok_and(|m| m.is_dir());
        clear(&d, &["rcds", "disc", "vecs"])
            + match nested {
                true => clear(&introspect, &["json"]),
                false => 0,
            }
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
