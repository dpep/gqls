//! Stderr verbosity, set once from `-q`/`-v` and shared across the CLI.
//!
//! Three levels: **quiet** (results + hard errors only), **normal** (status
//! chatter — a corrected kind or qualifier, no-matches), **verbose** (adds
//! diagnostics — cache hits, rq candidates). Status text goes through
//! [`status!`]/[`detail!`].

use std::sync::atomic::{AtomicU8, Ordering};

pub(crate) const QUIET: u8 = 0;
pub(crate) const NORMAL: u8 = 1;
pub(crate) const VERBOSE: u8 = 2;

static LEVEL: AtomicU8 = AtomicU8::new(NORMAL);

/// Set the global level from the parsed flags (called once at startup). `-v`
/// and `-q` are mutually exclusive at the clap layer, so at most one is set.
pub(crate) fn init(verbose: bool, quiet: bool) {
    let level = if verbose {
        VERBOSE
    } else if quiet {
        QUIET
    } else {
        NORMAL
    };
    LEVEL.store(level, Ordering::Relaxed);
}

pub(crate) fn level() -> u8 {
    LEVEL.load(Ordering::Relaxed)
}

pub(crate) fn is_quiet() -> bool {
    level() == QUIET
}

pub(crate) fn is_verbose() -> bool {
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
