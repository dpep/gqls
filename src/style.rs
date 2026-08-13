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

/// The answer to the question being asked — maximum contrast without choosing a
/// colour the background might swallow.
pub fn leaf(text: &str) -> String {
    paint(text, "1")
}

/// Structure the eye should skip: the parent prefix, argument lists, the arrow,
/// the kind tag, the description.
pub fn muted(text: &str) -> String {
    paint(text, "2")
}

/// A field's type. Cyan reads as "type" from syntax-highlighting convention and
/// stays legible on light and dark alike. Not bold — it must not outrank the
/// name.
pub fn type_name(text: &str) -> String {
    paint(text, "36")
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
        for f in [leaf, muted, type_name, warning] {
            assert_eq!(f("User"), "User");
        }
    }

    #[test]
    fn painting_never_changes_visible_length() {
        // The invariant the column layout depends on.
        for f in [leaf, muted, type_name, warning] {
            assert_eq!(f("Query.user").len(), "Query.user".len());
        }
    }
}
