//! Turning matches into text.
//!
//! Everything that decides how a result *looks*: the column layout, the
//! annotations on a record a query named, description wrapping, and the `-e`
//! draft's surrounding sections. Split out of `cli.rs`, which was 1800 lines of
//! argument parsing, dispatch and this — a size at which two doc comments had
//! already drifted onto the wrong items.
//!
//! The rule this module exists to hold: colour is additive, so every width is
//! measured on the plain text and applied through [`style::Line`]. Nothing here
//! may pad a styled string.

use anyhow::Result;
use serde::Serialize;

use crate::model::{Kind, SchemaRecord};
use crate::style;

/// A ranked result — from either the fuzzy scorer or the semantic ranker, so
/// both flow through one output path.
#[derive(Clone, Copy)]
pub struct Match<'a> {
    pub record: &'a SchemaRecord,
    pub score: f64,
}

/// Most cross-references to list before saying "and N more". A type used in
/// forty places has told you what you needed by the fifth.
const MAX_REFERENCES: usize = 6;

/// One value of an enum, as an explanation reports it.
#[derive(Serialize)]
pub struct EnumValue<'a> {
    name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<&'a str>,
    /// Present when deprecated; the string is the reason, empty if none was
    /// given. `Option<Option<_>>` would be the honest type and a bad one to
    /// read, so an empty reason stands for "deprecated, unexplained".
    #[serde(skip_serializing_if = "Option::is_none")]
    deprecated: Option<&'a str>,
}

/// What an explanation knows that isn't already a field on the record.
///
/// Computed once and rendered twice — as annotation lines for a person, as
/// extra keys for `--json`. Two derivations of the same facts would drift, and
/// the text output being richer than the machine output is backwards.
#[derive(Serialize, Default)]
pub struct Extras<'a> {
    /// An enum's values, so reading one doesn't need a second search.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    values: Vec<EnumValue<'a>>,
    /// Every path whose type is this one — the schema's answer to "how do I get
    /// one of these". The one fact here a consumer can't cheaply recompute: it
    /// would have to pull every record and scan.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    referenced_by: Vec<&'a str>,
}

pub fn extras<'a>(record: &SchemaRecord, records: &'a [SchemaRecord]) -> Extras<'a> {
    let values = match record.kind {
        Kind::Enum => records
            .iter()
            .filter(|r| r.kind == Kind::EnumValue && r.parent.as_deref() == Some(&record.name))
            .map(|r| EnumValue {
                name: &r.name,
                description: r.description.as_deref(),
                deprecated: r.deprecated.as_deref(),
            })
            .collect(),
        _ => Vec::new(),
    };
    // Types only — a field is already reachable through the type it hangs off,
    // which its own path shows.
    let referenced_by = match record.parent.is_none() && record.kind != Kind::Directive {
        true => records
            .iter()
            .filter(|r| r.base_type() == Some(record.name.as_str()) && r.path != record.path)
            .map(|r| r.path.as_str())
            .collect(),
        false => Vec::new(),
    };
    Extras {
        values,
        referenced_by,
    }
}

/// The single-line annotations, in the order they're printed.
///
/// Lines before blocks. These form a table whose shared label column only reads
/// as one when its rows are contiguous, so an enum's values — five lines with
/// their own indent — go after all of them rather than in the slot where they'd
/// sit semantically, beside a union's `members`. When the values *do* fit on one
/// line they join the table, in exactly that slot.
pub fn annotations(record: &SchemaRecord, extras: &Extras, descriptions: bool) -> Vec<Note> {
    let mut out = Vec::new();
    let note = |label, value: String| Note { label, value };

    if let Some(reason) = record.deprecated.as_deref().filter(|r| !r.is_empty()) {
        out.push(note("deprecated", reason.to_string()));
    }
    // `@deprecated` is skipped: the line above already carries it, with the
    // reason, which is the part worth reading.
    let applied: Vec<&str> = record
        .directives
        .iter()
        .map(String::as_str)
        .filter(|d| !d.starts_with("@deprecated"))
        .collect();
    if !applied.is_empty() {
        out.push(note("directives", applied.join(" ")));
    }
    if !record.possible_types.is_empty() {
        let label = match record.kind {
            Kind::Union => "members",
            _ => "implemented by",
        };
        out.push(note(label, record.possible_types.join(", ")));
    }
    if !extras.values.is_empty() && !values_need_a_block(&extras.values, descriptions) {
        out.push(note("values", collapsed_values(&extras.values)));
    }
    if !extras.referenced_by.is_empty() {
        let total = extras.referenced_by.len();
        let shown = total.min(MAX_REFERENCES);
        let mut list = extras.referenced_by[..shown].join(", ");
        if total > shown {
            list.push_str(&format!(", and {} more", total - shown));
        }
        out.push(note("referenced by", list));
    }
    out
}

/// Whether an enum's values need the block form. Only when at least one of them
/// has something to say — otherwise it's a list of names, which is a table row.
pub fn values_need_a_block(values: &[EnumValue], descriptions: bool) -> bool {
    descriptions && values.iter().any(|v| v.description.is_some())
}

/// One annotation: a label, and the value it answers for.
pub struct Note {
    label: &'static str,
    value: String,
}

/// Print the annotation table — every label sharing one column, values wrapped
/// with a hanging indent under their own start.
pub fn print_notes(notes: &[Note]) {
    let label_w = notes.iter().map(|n| n.label.len()).max().unwrap_or(0);
    for note in notes {
        let indent = 2 + label_w + 2;
        let budget = style::width()
            .saturating_sub(indent)
            .max(MIN_DESCRIPTION_WIDTH);
        let mut lines = wrap(&note.value, budget, usize::MAX).into_iter();
        let mut line = style::Line::default();
        line.push("  ", style::answer);
        line.push(note.label, style::muted);
        line.pad_to(2 + label_w);
        line.gap();
        line.push(&lines.next().unwrap_or_default(), style::muted);
        println!("{}", line.finish());
        for cont in lines {
            println!("{}{}", " ".repeat(indent), style::muted(&cont));
        }
    }
}

/// An enum's values, one per line with what each means.
///
/// Its own block rather than an annotation row, because the descriptions are
/// the point: reading `Role` should answer what `ADMIN` grants without a second
/// search for `Role.`. When no value has a description — `-D`, or an enum
/// nobody documented — there's nothing to lay out, so it collapses into the
/// table above instead (see [`collapsed_values`]).
pub fn print_values(values: &[EnumValue]) {
    println!("  {}", style::muted("values"));
    let name_w = values
        .iter()
        .map(|v| v.name.chars().count())
        .max()
        .unwrap_or(0);
    for value in values {
        let indent = 4 + name_w + 2;
        let budget = style::width()
            .saturating_sub(indent)
            .max(MIN_DESCRIPTION_WIDTH);
        // Deprecation gets the warning colour here too. In a twenty-value enum
        // the one value you must not use shouldn't be the least visible thing
        // on screen.
        let marker = match value.deprecated {
            // An empty reason still has to say *that* it's deprecated.
            Some("") => "(deprecated)".to_string(),
            Some(reason) => format!("(deprecated: {reason})"),
            None => String::new(),
        };
        let mut text = marker.clone();
        if let Some(d) = value.description {
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(d);
        }
        let mut lines = wrap(&text, budget, usize::MAX).into_iter();
        let first = lines.next().unwrap_or_default();

        let mut line = style::Line::default();
        line.push("    ", style::answer);
        line.push(value.name, style::name);
        line.pad_to(4 + name_w);
        line.gap();
        // The marker is only styled apart when it's whole on this line; a
        // reason long enough to wrap would otherwise leave a dangling colour.
        match first.starts_with(&marker) && !marker.is_empty() {
            true => {
                line.push(&marker, style::warning);
                line.push(&first[marker.len()..], style::muted);
            }
            false => line.push(&first, style::muted),
        }
        println!("{}", line.finish());
        for cont in lines {
            println!("{}{}", " ".repeat(indent), style::muted(&cont));
        }
    }
}

/// An enum's values on one line, for when none of them carries a description.
pub fn collapsed_values(values: &[EnumValue]) -> String {
    values
        .iter()
        .map(|v| match v.deprecated {
            Some(_) => format!("{} (deprecated)", v.name),
            None => v.name.to_string(),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Lines a description may occupy in a *list*, its first included.
///
/// One, because a list is for finding and a description there is a
/// disambiguator — enough to tell this row from that one. It used to be the
/// only place documentation appeared, which is why it used to be three; now
/// naming the record shows all of it, unwrapped and uncapped. Three lines of
/// ribbon in a 39-column gutter is the same shape the explained path exists to
/// avoid.
const DESCRIPTION_LINES: usize = 1;

/// Narrowest a description is worth printing. Below this it says nothing a
/// reader can use — `— Posts the…` is ten characters that disambiguate no rows
/// from each other — so a list drops it and lets the columns speak, and an
/// explanation lets its own block run past the edge instead of wrapping every
/// third word.
///
/// Twenty rather than something rounder: it's about three words, which is the
/// point where an elided description starts telling one row from the next.
const MIN_DESCRIPTION_WIDTH: usize = 20;

/// Widest the path column may grow before it stops aligning. One pathological
/// 200-character path shouldn't indent every other row past the fold, so it's
/// allowed to overflow its own line instead.
const PATH_WIDTH: usize = 48;

/// A row's cells, kept as plain text so the column widths can be measured, and
/// styled only on the way out.
pub struct Row {
    path: String,
    args: String,
    ret: String,
    kind: String,
    deprecated: bool,
    desc: String,
}

impl Row {
    /// Visible width of the path cell — the path and its arguments share one
    /// column. Counted in chars: GraphQL names are ASCII by spec, but the
    /// collapsed argument marker is an ellipsis, three bytes to one column.
    fn path_width(&self) -> usize {
        self.path.chars().count() + self.args.chars().count()
    }
}

pub fn print_text(matches: &[Match], descriptions: bool, explain: Option<&[SchemaRecord]>) {
    // With one result there is no table: nothing else pays for a long signature,
    // and there's no column to align a description against. Both of those turn
    // into "show the whole thing" below.
    let lone = matches.len() == 1;

    let rows: Vec<Row> = matches
        .iter()
        .map(|m| {
            let r = m.record;
            Row {
                path: r.path.clone(),
                // Collapsed in a list: the longest signature would otherwise
                // set the path column width for every row — 44 columns against
                // 22 on the example schema — and `-e` answers "how do I call
                // this" properly anyway. Alone, nothing else pays for it.
                args: match (r.args.is_empty(), lone) {
                    (true, _) => String::new(),
                    (false, true) => format!("({})", r.args.join(", ")),
                    (false, false) => "(…)".to_string(),
                },
                ret: r
                    .type_ref
                    .as_deref()
                    .map(|t| format!("-> {t}"))
                    .unwrap_or_default(),
                kind: format!("[{}]", r.kind.as_str()),
                deprecated: r.deprecated.is_some(),
                desc: match descriptions {
                    true => r.description.clone().unwrap_or_default(),
                    false => String::new(),
                },
            }
        })
        .collect();

    // Measure every column before printing any of it. A column that's empty
    // across the whole result set is dropped rather than left as a blank gutter
    // — a search returning only types has no return-type column at all.
    let path_w = rows
        .iter()
        .map(Row::path_width)
        .max()
        .unwrap_or(0)
        .min(PATH_WIDTH);
    let ret_w = rows.iter().map(|r| r.ret.len()).max().unwrap_or(0);
    let kind_w = rows.iter().map(|r| r.kind.len()).max().unwrap_or(0);

    for row in &rows {
        let mut line = style::Line::default();
        line.push(&row.path, style::name);
        line.push(&row.args, style::muted);
        if ret_w > 0 {
            line.pad_to(path_w);
            line.gap();
            line.push(&row.ret, style::answer);
        }
        if kind_w > 0 {
            line.pad_to(if ret_w > 0 { ret_w } else { path_w });
            line.gap();
            line.push(&row.kind, style::muted);
        }
        // Appended rather than given a column of its own: one deprecated row
        // would otherwise widen the kind column by 13 for every row.
        if row.deprecated {
            line.pad_to(kind_w);
            line.gap();
            line.push("(deprecated)", style::warning);
        }

        // A lone result gets the whole description, at full width, on its own
        // lines. Not just an uncapped version of the inline form: hanging the
        // full text off a 64-column indent wraps it into a tall ribbon a few
        // words wide, which is worse than the truncation it replaces.
        if lone {
            println!("{}", line.finish());
            for l in wrap(&row.desc, style::width().saturating_sub(2), usize::MAX) {
                println!("  {}", style::answer(&l));
            }
            if let Some(all) = explain {
                let extras = extras(matches[0].record, all);
                let notes = annotations(matches[0].record, &extras, descriptions);
                let block = values_need_a_block(&extras.values, descriptions);
                // One blank line, and only between two things worth separating:
                // prose above, a fact table below, at the same indent. Without
                // it a wrapped description's last line is indistinguishable from
                // the first annotation. `-e` separates its own sections the same
                // way. Never when either side is empty — a separator with
                // nothing on one side of it is just a gap.
                if !row.desc.is_empty() && (!notes.is_empty() || block) {
                    println!();
                }
                print_notes(&notes);
                if block {
                    print_values(&extras.values);
                }
            }
            continue;
        }

        if row.desc.is_empty() {
            println!("{}", line.finish());
            continue;
        }

        // Otherwise it hangs off the row, its continuations indented to where
        // it starts so a wrapped description reads as one block rather than as
        // a new result at column 0.
        line.pad_to(if kind_w > 0 { kind_w } else { path_w });
        line.gap();
        let indent = line.width();
        // What's actually left, not a floor. A row whose columns have eaten the
        // terminal has no room for a description, and forcing one produces
        // `— Posts the…`: ten characters that disambiguate nothing, on a line
        // that overflows anyway. Drop it and let the columns speak.
        let budget = style::width().saturating_sub(indent);
        if budget < MIN_DESCRIPTION_WIDTH {
            println!("{}", line.finish());
            continue;
        }
        // "— " belongs to the first line only; continuations align past it, so
        // the prose edges line up rather than the dash.
        let mut lines = wrap(&row.desc, budget.saturating_sub(2), DESCRIPTION_LINES).into_iter();
        if let Some(first) = lines.next() {
            line.push(&format!("— {first}"), style::muted);
        }
        println!("{}", line.finish());
        for cont in lines {
            println!("{}{}", " ".repeat(indent + 2), style::muted(&cont));
        }
    }
}

/// Break `text` into at most `max_lines` lines of `width` columns, on word
/// boundaries, marking the end with `…` when there was more.
///
/// A word longer than the whole width (a URL, a long type name) is left to
/// overflow rather than cut mid-token: breaking it produces two fragments that
/// are each unsearchable, and the thing that overflows is a dim tail.
pub fn wrap(text: &str, width: usize, max_lines: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();

    for word in text.split_whitespace() {
        let extra = if current.is_empty() {
            word.chars().count()
        } else {
            word.chars().count() + 1
        };
        if !current.is_empty() && current.chars().count() + extra > width {
            if lines.len() + 1 == max_lines {
                // No room for another line: elide. Making space for the marker
                // drops whole words, never part of one — `rather…` reads as
                // elided text, `rather tha…` reads as a bug.
                let mut kept = current;
                while kept.chars().count() + 1 > width {
                    match kept.rfind(' ') {
                        Some(i) => kept.truncate(i),
                        // A single word wider than the budget: there's no
                        // boundary to retreat to, so cut it.
                        None => {
                            kept.pop();
                        }
                    }
                }
                lines.push(format!("{}…", kept.trim_end()));
                return lines;
            }
            lines.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

pub fn render_example(example: &crate::example::Example) -> Result<String> {
    let mut out = String::new();

    // What the field does, above the operation that calls it. `-e` has already
    // committed to one field — it refuses to draft unless the query named it —
    // so there's no list to keep short here, and the whole description goes in.
    // A comment, like every other annotation in this output, so the block stays
    // pasteable in one go.
    if let Some(description) = example.description.as_deref() {
        for line in wrap(description, style::width().saturating_sub(2), usize::MAX) {
            out.push_str(&format!("# {line}\n"));
        }
        out.push('\n');
    }
    out.push_str(&example.operation);

    if !example.optional.is_empty() {
        out.push_str("\n# optional arguments:\n");
        for arg in &example.optional {
            out.push_str(&format!("#   {arg}\n"));
        }
    }
    if !example.input_types.is_empty() {
        out.push_str("\n# input types:\n");
        for line in example.input_types.iter().flatten() {
            out.push_str(&format!("#   {line}\n"));
        }
    }
    // An operation with no required arguments takes no variables; printing an
    // empty `{}` under a heading only invites the reader to look for something.
    if example.variables.as_object().is_some_and(|v| !v.is_empty()) {
        out.push_str("\n# variables\n");
        out.push_str(&format!(
            "{}\n",
            serde_json::to_string_pretty(&example.variables)?
        ));
    }
    // Every root that reaches the target, the drafted one first — so the pick
    // reads as a choice among the paths rather than as the only one. A single
    // path is already shown by the operation itself, and one entry under a
    // heading says nothing the draft didn't.
    let paths = example.paths();
    if paths.len() > 1 {
        out.push_str("\n# paths:\n");
        for path in paths {
            out.push_str(&format!("#   {path}\n"));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::{render_example, wrap};
    use crate::example::Example;

    fn example() -> Example {
        Example {
            operation: "query Users {\n  users {\n    email\n  }\n}\n".to_string(),
            description: None,
            variables: serde_json::json!({}),
            optional: Vec::new(),
            input_types: Vec::new(),
            deprecated: Vec::new(),
            via: None,
            alternatives: Vec::new(),
        }
    }

    #[test]
    fn collapses_block_descriptions_to_one_line() {
        assert_eq!(
            wrap("  Look up\n  a user.\n", 40, 3),
            vec!["Look up a user."]
        );
        assert!(wrap("   ", 40, 3).is_empty());
    }

    #[test]
    fn wraps_on_word_boundaries_within_the_budget() {
        let out = wrap("the quick brown fox jumps over the lazy dog", 12, 9);
        assert!(
            out.iter().all(|l| l.chars().count() <= 12),
            "over budget: {out:?}"
        );
        assert_eq!(out.join(" "), "the quick brown fox jumps over the lazy dog");
    }

    #[test]
    fn elides_once_it_runs_out_of_lines() {
        let long = "word ".repeat(60);
        let out = wrap(&long, 20, 3);
        assert_eq!(out.len(), 3, "{out:?}");
        assert!(out.last().unwrap().ends_with('…'), "{out:?}");
        // The marker has to fit the budget, not sit one column past it.
        assert!(
            out.iter().all(|l| l.chars().count() <= 20),
            "over budget: {out:?}"
        );
    }

    #[test]
    fn a_description_that_fits_stays_on_one_line() {
        let out = wrap("An account.", 40, 3);
        assert_eq!(out, vec!["An account.".to_string()]);
    }

    #[test]
    fn a_word_longer_than_the_budget_overflows_rather_than_splitting() {
        // Splitting a URL or a long type name yields two unsearchable halves.
        let out = wrap("see https://example.com/a/very/long/path now", 12, 3);
        assert!(
            out.iter().any(|l| l.contains("https://example.com")),
            "{out:?}"
        );
    }

    #[test]
    fn an_operation_with_nothing_to_add_is_printed_alone() {
        let ex = example();
        assert_eq!(render_example(&ex).unwrap(), ex.operation);
    }

    #[test]
    fn a_description_heads_the_draft_as_a_comment() {
        let mut ex = example();
        ex.description = Some("Look up a user by id.".to_string());
        let out = render_example(&ex).unwrap();
        assert!(
            out.starts_with("# Look up a user by id.\n\nquery Users {"),
            "{out}"
        );
        // Commented, so the whole block still pastes as one document.
        graphql_parser::parse_query::<String>(&out).expect("draft should stay valid");
    }

    #[test]
    fn a_long_description_is_wrapped_and_every_line_commented() {
        let mut ex = example();
        ex.description = Some("word ".repeat(60));
        let out = render_example(&ex).unwrap();
        let heading: Vec<&str> = out.lines().take_while(|l| l.starts_with('#')).collect();
        assert!(heading.len() > 3, "expected a wrapped block: {out}");
        // Uncapped — `-e` has already committed to one field, so there's no
        // list for a long description to bury.
        assert!(!out.contains('…'), "should not elide in a draft: {out}");
        graphql_parser::parse_query::<String>(&out).expect("draft should stay valid");
    }

    #[test]
    fn variables_are_printed_only_when_there_are_some() {
        let mut ex = example();
        ex.variables = serde_json::json!({ "id": "<ID!>" });
        let out = render_example(&ex).unwrap();
        assert!(
            out.contains("# variables\n{\n  \"id\": \"<ID!>\"\n}\n"),
            "{out}"
        );
    }

    #[test]
    fn the_roots_that_reach_the_target_are_listed_with_the_drafted_one_first() {
        let mut ex = example();
        ex.via = Some("Query.users".to_string());
        ex.alternatives = vec!["Query.user".to_string()];
        let out = render_example(&ex).unwrap();
        assert!(
            out.ends_with("\n# paths:\n#   Query.users\n#   Query.user\n"),
            "{out}"
        );
    }

    #[test]
    fn a_lone_path_is_left_to_the_operation_to_show() {
        let mut ex = example();
        ex.via = Some("Query.users".to_string());
        let out = render_example(&ex).unwrap();
        assert!(!out.contains("# paths"), "{out}");
    }
}
