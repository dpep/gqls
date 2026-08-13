//! Column layout of the text output. Drives the real binary, because the thing
//! under test is what a person sees in a terminal — and because the styling
//! decides whether to emit escapes by asking whether stdout is a TTY, which a
//! library test can't exercise.
//!
//! These pin the *uncoloured* rendering, which is what a pipe, `NO_COLOR`, and
//! the AI reading this output in a Claude Code session all get. Colour is
//! additive by design: it never changes a visible character, so if the plain
//! layout is right the coloured one is too.

mod common;

use std::process::Command;

const SCHEMA: &str = "examples/schema.graphql";

fn run(args: &[&str]) -> String {
    common::assert_binary_is_current(env!("CARGO_BIN_EXE_gqls"));
    let out = Command::new(env!("CARGO_BIN_EXE_gqls"))
        // Keep this off the embedding model: layout is about the columns, not
        // the ranking, and a semantic build shouldn't change what's measured.
        .arg("--fuzzy")
        .args(args)
        .arg(SCHEMA)
        .output()
        .expect("gqls should run");
    String::from_utf8(out.stdout).expect("output should be utf-8")
}

/// The column a substring starts in, or `None` if the line doesn't contain it.
///
/// Columns, not bytes. `str::find` returns a byte offset, and the collapsed
/// argument marker `(…)` is three bytes wide but one column wide — measuring in
/// bytes reports rows that *are* aligned as misaligned.
fn column_of(line: &str, needle: &str) -> Option<usize> {
    line.find(needle).map(|b| line[..b].chars().count())
}

/// Column of the `[kind]` tag's opening bracket.
///
/// Not `find("[")`: GraphQL list syntax uses brackets too, so `-> [User!]!`
/// would match first. A kind tag is the only bracket group that is entirely
/// lowercase and underscores.
fn kind_column(line: &str) -> Option<usize> {
    line.char_indices()
        .find(|&(i, c)| {
            c == '[' && {
                let rest = &line[i + 1..];
                match rest.find(']') {
                    Some(end) => {
                        !rest[..end].is_empty()
                            && rest[..end]
                                .chars()
                                .all(|c| c.is_ascii_lowercase() || c == '_')
                    }
                    None => false,
                }
            }
        })
        .map(|(b, _)| line[..b].chars().count())
}

#[test]
fn kind_tags_line_up_across_rows() {
    // The bug this replaced: a record with no return type still paid for the
    // separator that `-> Type` would have used, so `[object]` landed one column
    // off from every `[query]` and the eye had nothing to run down.
    let out = run(&["*", "-l", "20"]);
    let columns: Vec<usize> = out.lines().filter_map(kind_column).collect();
    assert!(columns.len() > 3, "need several rows to compare: {out}");
    let first = columns[0];
    assert!(
        columns.iter().all(|c| *c == first),
        "kind tags should share a column, got {columns:?} in:\n{out}"
    );
}

#[test]
fn return_types_line_up_across_rows() {
    let out = run(&["User."]);
    let arrows: Vec<usize> = out.lines().filter_map(|l| column_of(l, "-> ")).collect();
    assert!(arrows.len() > 3, "need several rows: {out}");
    assert!(
        arrows.windows(2).all(|w| w[0] == w[1]),
        "arrows should share a column, got {arrows:?} in:\n{out}"
    );
}

#[test]
fn a_column_nothing_fills_is_dropped_entirely() {
    // Objects have no return type. The arrow column shouldn't survive as a
    // blank gutter pushing every kind tag right.
    let out = run(&["*", "-k", "object"]);
    assert!(!out.contains("->"), "no arrows expected in:\n{out}");
    let kinds: Vec<usize> = out.lines().filter_map(kind_column).collect();
    assert!(!kinds.is_empty(), "expected object rows: {out}");
    // Exactly two spaces past the widest name — not two plus a dead column.
    let longest = out
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .map(|p| p.chars().count())
        .max()
        .unwrap();
    assert_eq!(
        kinds[0],
        longest + 2,
        "kind should sit two spaces past the widest name in:\n{out}"
    );
}

#[test]
fn a_multibyte_marker_does_not_skew_the_columns() {
    // `(…)` is three bytes and one column. Padding computed in bytes would
    // pull every row that takes arguments two columns left of the rest — and
    // it's an easy mistake, because the whole path column is otherwise ASCII.
    let out = run(&["User."]);
    assert!(
        out.contains("(…)"),
        "expected a collapsed signature in:\n{out}"
    );
    let arrows: Vec<usize> = out.lines().filter_map(|l| column_of(l, "-> ")).collect();
    assert!(
        arrows.windows(2).all(|w| w[0] == w[1]),
        "rows with and without arguments should align, got {arrows:?} in:\n{out}"
    );
}

#[test]
fn a_signature_is_collapsed_not_spelled_out() {
    // The list answers "which field"; `-e` answers "how do I call it". One long
    // signature would otherwise set the path column width for every row.
    let out = run(&["*", "-l", "20"]);
    assert!(
        !out.contains("input: CreateUserInput!"),
        "argument signatures should not reach the list:\n{out}"
    );
}

#[test]
fn no_line_ends_in_whitespace() {
    // Padding the last cell would leave invisible trailing spaces that break
    // diffs and `grep -c ' $'` style checks.
    for args in [
        &["*", "-l", "20"][..],
        &["User."][..],
        &["*", "-k", "object"][..],
    ] {
        let out = run(args);
        for line in out.lines() {
            assert_eq!(line, line.trim_end(), "trailing whitespace in {args:?}");
        }
    }
}

#[test]
fn piped_output_carries_no_escapes() {
    // The contract that lets one code path serve a terminal and a pipe.
    for args in [&["*", "-l", "20"][..], &["User."][..]] {
        let out = run(args);
        assert!(
            !out.contains('\x1b'),
            "escape sequences leaked into piped output for {args:?}:\n{out}"
        );
    }
}
