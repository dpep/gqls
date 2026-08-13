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
    run_against(SCHEMA, args)
}

fn run_against(schema: &str, args: &[&str]) -> String {
    common::assert_binary_is_current(env!("CARGO_BIN_EXE_gqls"));
    let out = Command::new(env!("CARGO_BIN_EXE_gqls"))
        // Keep this off the embedding model: layout is about the columns, not
        // the ranking, and a semantic build shouldn't change what's measured.
        .arg("--fuzzy")
        .args(args)
        .arg(schema)
        .output()
        .expect("gqls should run");
    String::from_utf8(out.stdout).expect("output should be utf-8")
}

/// Just the result rows. A wrapped description continues on indented lines,
/// which are part of the row above rather than results of their own — counting
/// them as results, or measuring a path column against them, is the mistake
/// this exists to prevent.
fn rows(out: &str) -> Vec<&str> {
    out.lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with(' '))
        .collect()
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
    let columns: Vec<usize> = rows(&out).into_iter().filter_map(kind_column).collect();
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
    let arrows: Vec<usize> = rows(&out)
        .into_iter()
        .filter_map(|l| column_of(l, "-> "))
        .collect();
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
    let kinds: Vec<usize> = rows(&out).into_iter().filter_map(kind_column).collect();
    assert!(!kinds.is_empty(), "expected object rows: {out}");
    // Exactly two spaces past the widest name — not two plus a dead column.
    let longest = rows(&out)
        .into_iter()
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
fn kind_tags_align_when_there_is_no_return_column() {
    // Dropping the empty arrow column shifts kind into the slot the arrow held,
    // so a width array indexed by position pads it to the arrow's width — zero
    // — and every description starts somewhere different. Invisible on the
    // main fixture, where some record always has a return type.
    let out = run_against("tests/fixtures/no_return_types.graphql", &["*"]);
    assert!(
        !out.contains("->"),
        "fixture should have no return types:\n{out}"
    );
    let kinds: Vec<usize> = rows(&out).into_iter().filter_map(kind_column).collect();
    assert!(kinds.len() > 2, "need several rows: {out}");
    assert!(
        kinds.iter().all(|c| *c == kinds[0]),
        "kind tags should share a column, got {kinds:?} in:\n{out}"
    );
    // And so should the descriptions hanging off them.
    let dashes: Vec<usize> = rows(&out)
        .into_iter()
        .filter_map(|l| column_of(l, "— "))
        .collect();
    assert!(
        dashes.iter().all(|c| *c == dashes[0]),
        "descriptions should share a column, got {dashes:?} in:\n{out}"
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
    let arrows: Vec<usize> = rows(&out)
        .into_iter()
        .filter_map(|l| column_of(l, "-> "))
        .collect();
    assert!(
        arrows.windows(2).all(|w| w[0] == w[1]),
        "rows with and without arguments should align, got {arrows:?} in:\n{out}"
    );
}

#[test]
fn a_signature_is_collapsed_among_other_rows() {
    // The list answers "which field"; `-e` answers "how do I call it". One long
    // signature would otherwise set the path column width for every row.
    let out = run(&["*", "-l", "20"]);
    assert!(rows(&out).len() > 1, "expected a list: {out}");
    assert!(
        !out.contains("input: CreateUserInput!"),
        "argument signatures should not reach a multi-row list:\n{out}"
    );
}

#[test]
fn a_lone_result_spells_its_signature_out() {
    // Collapsing buys back width from the *other* rows. With one row there are
    // none, so the marker would cost information and save nothing.
    let out = run(&["Mutation.createUser"]);
    assert_eq!(rows(&out).len(), 1, "expected exactly one row: {out}");
    assert!(
        out.contains("(input: CreateUserInput!)"),
        "a lone result should show its arguments:\n{out}"
    );
}

#[test]
fn a_long_description_wraps_within_the_fallback_width() {
    // Piped output has no terminal to measure, so it uses the fixed fallback.
    // Before wrapping, a documented row ran to 112 columns and the terminal
    // broke it at column 0 — right where the next result's name starts, which
    // is exactly the alignment the columns exist to provide.
    let out = run(&["User."]);
    for line in out.lines() {
        assert!(
            line.chars().count() <= 80,
            "line over the fallback width ({}): {line}",
            line.chars().count()
        );
    }
}

#[test]
fn a_wrapped_description_is_indented_past_its_row() {
    // A continuation at column 0 would read as a new result. A list caps
    // descriptions at one line now, so the wrap worth checking is an
    // explanation's, where the text runs at full width beneath its row.
    let out = run(&["Query.users", "-k", "query"]);
    let continuations: Vec<&str> = out
        .lines()
        .filter(|l| l.starts_with(' ') && !l.trim().is_empty())
        .collect();
    assert!(continuations.len() > 1, "expected a wrap in:\n{out}");
    assert!(
        continuations.iter().all(|l| l.starts_with("  ")),
        "continuations should be indented:\n{out}"
    );
}

#[test]
fn a_description_is_capped_rather_than_running_on() {
    // A list is for finding, so a description there is one line — enough to
    // tell this row from that one, and no more. `Query.` is a genuine list: a
    // trailing dot enumerates and names nothing, where a query that *named* a
    // record would explain it instead, uncapped.
    let out = run(&["Query."]);
    assert!(rows(&out).len() > 3, "expected a list:\n{out}");
    assert_eq!(
        out.lines().count(),
        rows(&out).len(),
        "no row in a list should wrap:\n{out}"
    );
    assert!(out.contains('…'), "expected an elision:\n{out}");
}

#[test]
fn a_lone_result_gets_its_whole_description_at_full_width() {
    // `Query.users` has the longest doc in the fixture; in a list it's cut to
    // three lines. Alone, nothing is competing for the space, so it's shown
    // whole — and as its own block rather than hanging off a 60-column indent,
    // which would wrap it into a ribbon a few words wide.
    let out = run(&["Query.users", "-k", "query", "-l", "1"]);
    assert_eq!(rows(&out).len(), 1, "expected one row:\n{out}");
    assert!(!out.contains('…'), "should not elide a lone result:\n{out}");
    assert!(
        out.contains("maximum instead of an error."),
        "expected the tail of the description:\n{out}"
    );
    // Its own block: indented two, not aligned under a description column.
    let continuations: Vec<&str> = out.lines().filter(|l| l.starts_with(' ')).collect();
    assert!(
        continuations
            .iter()
            .all(|l| l.len() - l.trim_start().len() == 2),
        "expected a two-space block:\n{out}"
    );
}

#[test]
fn capitalisation_picks_the_type_out_of_its_fields() {
    // `Role` matches the enum, `User.role` and `CreateUserInput.role`, and names
    // only the enum — GraphQL capitalises types and doesn't capitalise fields,
    // so case is real evidence of which one was meant.
    let out = run(&["Role"]);
    assert_eq!(rows(&out).len(), 1, "expected just the enum:\n{out}");
    assert!(out.starts_with("Role  [enum]"), "{out}");
    assert!(out.contains("values"), "expected annotations:\n{out}");
}

#[test]
fn the_lowercase_query_stays_a_search() {
    let out = run(&["role"]);
    assert!(rows(&out).len() > 1, "expected a list:\n{out}");
    assert!(!out.contains("values  "), "should not annotate:\n{out}");
}

#[test]
fn naming_two_records_is_still_a_search() {
    // `email` names `User.email` and `CreateUserInput.email` equally well.
    // Picking one to explain would answer a question that isn't finished.
    let out = run(&["email"]);
    assert!(rows(&out).len() > 1, "expected a list:\n{out}");
    assert!(!out.contains("directives"), "should not annotate:\n{out}");
}

#[test]
fn no_explain_forces_the_list_back() {
    let explained = run(&["Role"]);
    let listed = run(&["Role", "--no-explain"]);
    assert_eq!(rows(&explained).len(), 1);
    assert!(rows(&listed).len() > 1, "expected every match:\n{listed}");
}

#[test]
fn an_annotation_carries_what_the_row_cannot() {
    // The deprecation *reason* had been in the JSON and nowhere in the text.
    let out = run(&["Mutation.deleteUser"]);
    assert!(out.contains("deprecated  use archiveUser"), "{out}");
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
