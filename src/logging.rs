//! Stderr verbosity, set once from `-q`/`-v` and shared across the CLI.
//!
//! Three levels: **quiet** (results + hard errors only), **normal** (status
//! chatter — which schema, warming, no-matches), **verbose** (adds diagnostics
//! — cache hits, rq candidates, why the embedding model loaded or fell back).
//! Status text goes through [`status!`]/[`detail!`]; under the semantic feature
//! we also route the borrowed `log::` macros here, so `-v` surfaces the
//! otherwise-silent model-load `debug!`/`warn!` lines from `ae`'s pipeline.

use std::sync::atomic::{AtomicU8, Ordering};

pub const QUIET: u8 = 0;
pub const NORMAL: u8 = 1;
pub const VERBOSE: u8 = 2;

static LEVEL: AtomicU8 = AtomicU8::new(NORMAL);

/// Set the global level from the parsed flags (called once at startup). `-v`
/// and `-q` are mutually exclusive at the clap layer, so at most one is set.
pub fn init(verbose: bool, quiet: bool) {
    let level = if verbose {
        VERBOSE
    } else if quiet {
        QUIET
    } else {
        NORMAL
    };
    LEVEL.store(level, Ordering::Relaxed);
    #[cfg(feature = "_semantic")]
    init_log_backend(level);
}

pub fn level() -> u8 {
    LEVEL.load(Ordering::Relaxed)
}

pub fn is_quiet() -> bool {
    level() == QUIET
}

pub fn is_verbose() -> bool {
    level() >= VERBOSE
}

/// Normal status chatter — printed unless `-q`. Prefixed `gqls:`.
#[macro_export]
macro_rules! status {
    ($($arg:tt)*) => {
        if $crate::logging::level() >= $crate::logging::NORMAL {
            eprintln!("gqls: {}", format_args!($($arg)*));
        }
    };
}

/// Verbose-only diagnostics — printed only under `-v`. Prefixed `gqls:`.
#[macro_export]
macro_rules! detail {
    ($($arg:tt)*) => {
        if $crate::logging::is_verbose() {
            eprintln!("gqls: {}", format_args!($($arg)*));
        }
    };
}

/// Route the `log::` macros from the borrowed embedding code to stderr, gated by
/// our level: quiet silences them, normal shows warnings (real failures like a
/// failed tokenize/inference), verbose shows the debug lines that explain *why*
/// the ONNX model loaded or fell back to the hash embedder.
#[cfg(feature = "_semantic")]
fn init_log_backend(level: u8) {
    use log::LevelFilter;

    static LOGGER: StderrLogger = StderrLogger;
    let filter = match level {
        QUIET => LevelFilter::Off,
        VERBOSE => LevelFilter::Debug,
        _ => LevelFilter::Warn,
    };
    // Only the first call can install the logger; ignore a repeat (e.g. tests).
    let _ = log::set_logger(&LOGGER);
    log::set_max_level(filter);
}

#[cfg(feature = "_semantic")]
struct StderrLogger;

#[cfg(feature = "_semantic")]
impl log::Log for StderrLogger {
    fn enabled(&self, meta: &log::Metadata) -> bool {
        // Only our own crate's diagnostics (including the borrowed `ae` embedding
        // code, which compiles under the `gqls` crate). Without this, `-v` would
        // also dump ureq/rustls TLS-handshake debug spam when introspecting a URL.
        let t = meta.target();
        t == "gqls" || t.starts_with("gqls::")
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            eprintln!(
                "gqls: [{}] {}",
                record.level().as_str().to_ascii_lowercase(),
                record.args()
            );
        }
    }

    fn flush(&self) {}
}
