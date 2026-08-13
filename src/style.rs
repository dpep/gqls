//! ANSI styling for text output.
//!
//! Colour here is strictly *additive*: the visible characters are identical
//! with and without it, so column widths are computed once, from the plain
//! text, and the escapes are wrapped around the result. That rule is what lets
//! the same code path serve a terminal, a pipe, and the AI reading this output
//! in a Claude Code session — nobody gets a different layout, only a different
//! amount of ink.
//!
//! Sixteen-colour codes only, and no background colours: a 256-colour palette
//! picks fights with whatever theme the user actually has, and anything that
//! assumes a dark background is unreadable for half of everyone. Cyan is the
//! one hue used, because it's mid-luminance in essentially every theme and so
//! survives white and black alike. The rest is bold and dim, which are relative
//! to the user's own foreground and therefore can't clash.

use std::io::IsTerminal;
use std::sync::OnceLock;

/// Whether to emit escapes at all: a TTY, and `NO_COLOR` unset.
///
/// Checked once — the answer can't change mid-run, and this is called per cell
/// of every row.
pub fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED
        .get_or_init(|| std::env::var_os("NO_COLOR").is_none() && std::io::stdout().is_terminal())
}

/// Fallback width when there's no terminal to ask — a pipe, a file, a CI log.
///
/// Fixed rather than unlimited, because something has to bound a description,
/// and fixed rather than "whatever the terminal was" because piped output has
/// to be reproducible: the test suite and any diff of two runs depend on the
/// same input producing the same bytes.
pub const FALLBACK_WIDTH: usize = 80;

/// Columns available for output.
///
/// Asked of the terminal directly rather than read from `$COLUMNS`, which
/// shells often don't export to child processes — it's set in the parent and
/// simply absent here, so trusting it silently gives every piped run the wrong
/// answer.
pub fn width() -> usize {
    static WIDTH: OnceLock<usize> = OnceLock::new();
    *WIDTH.get_or_init(|| {
        if !std::io::stdout().is_terminal() {
            return FALLBACK_WIDTH;
        }
        terminal_size::terminal_size()
            .map(|(terminal_size::Width(w), _)| w as usize)
            .filter(|w| *w > 0)
            .unwrap_or(FALLBACK_WIDTH)
    })
}

/// Wrap `text` in `code`, or return it unchanged when colour is off.
///
/// Always resets fully (`\x1b[0m`) rather than turning off the one attribute:
/// `\x1b[22m` clears bold *and* dim together, which goes wrong the moment two
/// styled spans sit next to each other — as the parent/leaf pair does.
fn paint(text: &str, code: &str) -> String {
    if text.is_empty() || !enabled() {
        return text.to_string();
    }
    format!("\x1b[{code}m{text}\x1b[0m")
}

/// The name you searched for — the whole path, `User.posts`, as one thing.
///
/// Not split into a dim parent and a bold leaf. That reads well down a column
/// of five `User.` rows and badly everywhere else: on a single result it
/// fragments the one string you're looking at into two shades for no gain, and
/// what you matched on was the whole path anyway.
pub fn name(text: &str) -> String {
    paint(text, "1")
}

/// Everything that isn't the name: arguments, the arrow, the return type, the
/// kind tag, the description.
///
/// One treatment for all of it, deliberately. An earlier version gave the
/// return type its own colour, which meant three visual weights on a line whose
/// job is to answer one question. Dim-versus-not is the whole distinction the
/// eye needs here, and it's the only one that can't clash with a terminal
/// theme — bold and dim are relative to the user's own foreground, where any
/// hue is a guess about their background *and* their palette.
pub fn muted(text: &str) -> String {
    paint(text, "2")
}

/// Deprecation. Rare and semantic, so it gets the one colour that interrupts a
/// scan. Plain red, not bright red, which washes out on light backgrounds.
pub fn warning(text: &str) -> String {
    paint(text, "31")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Colour is decided by [`enabled`], which is false under `cargo test`
    /// (stdout isn't a TTY), so these assert the uncoloured contract — the one
    /// that pipes, `NO_COLOR`, and the test suite all rely on.
    #[test]
    fn styling_is_a_no_op_when_disabled() {
        assert!(!enabled(), "tests must run without a TTY");
        for f in [name, muted, warning] {
            assert_eq!(f("User"), "User");
        }
    }

    #[test]
    fn painting_never_changes_visible_length() {
        // The invariant the column layout depends on.
        for f in [name, muted, warning] {
            assert_eq!(f("Query.user").len(), "Query.user".len());
        }
    }
}
