//! Draft a ready-to-paste example operation for a matched field.
//!
//! Everything here is mechanical — the schema already says what the arguments
//! are, what the field returns, and which of that type's fields are leaves. The
//! rules, and why each one:
//!
//! * **Arguments you must supply become variables.** Never inline a literal
//!   into the query body; a pasted operation should be parameterized from the
//!   start. An argument the server can fill in for itself — nullable, or with
//!   a schema default — is left out of the operation entirely and listed
//!   underneath, so the query runs as-is and the knobs are still discoverable.
//! * **A placeholder names its type.** `"<ID!>"` says both what to put there
//!   and that it's required; `""` or `0` look like real values and get pasted
//!   by accident.
//! * **One level of selection by default, leaves only.** A scalar or enum
//!   return needs no selection set at all. An object return gets its scalar/enum
//!   fields, and a commented `# name: Type { … }` marker for the object-valued
//!   ones — guessing how deep someone wants to go is worse than leaving a hole,
//!   and `--depth` asks for more when you do want it. The marker stays a
//!   comment because there is no valid empty selection set: `author { ... }`,
//!   `author { … }` and `author {}` are all parse errors, and every draft this
//!   prints has to survive a paste.
//! * **Abstract types get inline fragments.** A union has no fields of its own,
//!   so it's written as `... on Member { … }` over its concrete types — the only
//!   form a server will accept.
//! * **Deprecated fields are flagged, not dropped.** They're still selected and
//!   marked `# deprecated: reason`, because silently omitting a field the schema
//!   still serves is its own surprise.
//! * **Schema prose in a draft is flattened to one line.** A `#` comment ends
//!   at the next line terminator, so a deprecation reason that spans lines
//!   would spill its tail into the selection set, where a server reads it as
//!   field names. Everything free-form goes through [`one_line`]; names and
//!   type references can't hold a newline and don't need it.
//! * **An `errors` block only when the schema has one.** The payload/errors
//!   convention is widespread but not universal, so it's expanded only when
//!   that field really exists.
//! * **A nested field is reached through a chain of fields from a root.**
//!   `Company.employee` isn't callable on its own, so it's nested inside
//!   whatever sequence of fields leads to a `Company`: one hop where a root
//!   returns one, and as many as it takes where a schema namespaces its roots
//!   (`Query.payroll: PayrollQueries`, with the real fields hanging off that).
//!   The shortest chain wins, then the fewest arguments to fill in; when
//!   several tie, the caller is told, rather than the pick being passed off as
//!   obvious. The same walk answers the other edge — an input object is
//!   reached by the chain leading to the field that *takes* it, or, where
//!   nothing takes it, to the fields taking the nearest inputs that *hold* it.
//!   Every such holder is offered, not just the drafted one: two inputs holding
//!   the same thing are two different things to pass, so the path names which.

use std::cell::OnceCell;
use std::collections::{HashMap, HashSet};

use anyhow::{bail, Result};
use serde_json::{Map, Value};

use crate::model::{base_of, split_arg, Arg, Kind, SchemaRecord};

/// One documented argument of a drafted operation.
#[derive(Debug, serde::Serialize)]
pub struct ArgDoc {
    pub name: String,
    pub description: String,
}

/// A drafted operation and the variables it expects.
#[derive(Debug)]
pub struct Example {
    /// The GraphQL document, ready to paste.
    pub operation: String,
    /// What the schema says the target field does, if anything. Carried so the
    /// draft can lead with it: `-e` commits to one field, and what it does is
    /// worth knowing before you paste an operation that calls it.
    pub description: Option<String>,
    /// A JSON object of placeholder variable values, one per required argument.
    pub variables: Value,
    /// Arguments left out because the server can supply them — rendered as
    /// `field(name: Type = default)`, ready to paste back in.
    pub optional: Vec<String>,
    /// The arguments the schema documents. A signature says what to pass; this
    /// says what passing it does, which is the half a draft can't show by shape
    /// alone — `owner: String!` never says it wants a login.
    pub arguments: Vec<ArgDoc>,
    /// `filter: PostFilter` for each variable the skeleton expanded into an
    /// object — its JSON key alone no longer names its type.
    pub variable_types: Vec<String>,
    /// The enums the variables reach, `Role = ADMIN | MEMBER | GUEST`. JSON has
    /// no way to hold "one of these", so the choice is listed beside the block
    /// rather than inside it.
    pub enums: Vec<String>,
    /// Deprecated fields the draft touched — the target itself, or anything
    /// selected. Flagged inline too; this is for the caller to warn about.
    pub deprecated: Vec<String>,
    /// The root field a nested target was reached through, if it needed one.
    pub via: Option<String>,
    /// The input an argument actually carries, when the target only rides
    /// inside it — `AddressInput` passed as part of `AddressValidationInput`.
    /// `None` whenever the draft passes the target itself.
    pub through: Option<String>,
    /// Other root fields that could have reached a nested target. Non-empty
    /// only when the choice was ambiguous.
    pub alternatives: Vec<String>,
    /// Whether the selection drew no leaf at all — markers and a `__typename`,
    /// which runs and fetches nothing. True on every wrapper type, so the
    /// caller can point at `--depth` before the reader concludes there was
    /// nothing to select.
    pub no_leaves: bool,
}

impl Example {
    /// Every root that reaches the target, the one drafted through first.
    /// Empty for a root field, which is already callable on its own.
    pub fn paths(&self) -> Vec<&str> {
        self.via
            .iter()
            .chain(&self.alternatives)
            .map(String::as_str)
            .collect()
    }
}

/// Draft an operation that reaches `target`.
/// `depth` is how many levels of fields to select; `None` takes the default for
/// the kind of target — one for a field, and the barest valid selection for an
/// input object, whose draft is about the argument rather than the payload.
pub fn build(
    target: &SchemaRecord,
    records: &[SchemaRecord],
    depth: Option<usize>,
) -> Result<Example> {
    let schema = Schema::index(records);

    // The input actually carried by an argument, when the one asked about only
    // rides inside it. Set by the input arm below.
    let mut through: Option<String> = None;
    // The chain of fields to nest, outermost first, and the input type whose
    // argument the draft exists to show — `None` unless the target *is* that
    // input, in which case the argument carrying it must be supplied even where
    // the schema calls it optional.
    let (chain, via, alternatives, required) = match target.kind {
        Kind::Query | Kind::Mutation | Kind::Subscription => (vec![target], None, Vec::new(), None),
        Kind::Field => {
            let parent = target
                .parent
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("{} has no enclosing type", target.path))?;
            let (hops, mut chains) = schema.chains_reaching(parent, false);
            if chains.is_empty() {
                bail!("{} {}", target.path, out_of_reach(parent, hops));
            }
            let mut chain = chains.remove(0);
            let via = Some(label(&chain));
            let alternatives = chains.iter().map(|c| label(c)).collect();
            chain.push(target);
            (chain, via, alternatives, None)
        }
        // An input object is never callable, but it is always *passable*: the
        // question it answers is "where does this go", and the answer is the
        // field that takes it. So it's reached the same way a nested field is,
        // along the other edge — an argument of this type rather than a return
        // of it. An input field rides on its enclosing input object, which is
        // the thing an operation can actually name.
        Kind::InputObject | Kind::InputField => {
            let input = match target.kind {
                Kind::InputField => target
                    .parent
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("{} has no enclosing input", target.path))?,
                _ => target.name.as_str(),
            };
            let (levels, mut chains) = schema.chains_passing(input);
            if chains.is_empty() {
                bail!("{}", unpassable(input, levels));
            }
            let best = chains.remove(0);
            through = (best.held != input).then(|| best.held.to_string());
            let alternatives = chains.into_iter().map(|p| p.path).collect();
            (best.chain, Some(best.path), alternatives, Some(best.held))
        }
        // A type is not callable either, but asking for one is asking how to
        // fetch one — so it drafts the root that reaches it, narrowed to it
        // where the root returns something broader.
        Kind::Object | Kind::Interface | Kind::Union => {
            let (hops, mut chains) = schema.chains_reaching(&target.name, true);
            if chains.is_empty() {
                bail!("{} {}", target.name, out_of_reach(&target.name, hops));
            }
            let chain = chains.remove(0);
            let via = Some(label(&chain));
            let alternatives = chains.iter().map(|c| label(c)).collect();
            (chain, via, alternatives, None)
        }
        // An enum, a scalar, a directive: nothing you can select, and nothing
        // that carries an operation. What you *can* ask is which fields hand
        // one back.
        _ => {
            // The useful question about something unselectable is which fields
            // hand one back — asked about the type itself, which for an enum
            // value is the enum it belongs to. A directive is returned by
            // nothing, so it gets no pointer rather than a wrong one.
            let returns = match target.kind {
                Kind::Directive => None,
                Kind::EnumValue => target.parent.as_deref(),
                _ => Some(target.name.as_str()),
            };
            let hint = returns
                .map(|t| format!(" Try `gqls --returns {t}` for the fields that give one back."))
                .unwrap_or_default();
            bail!("{} can't be selected by an operation.{hint}", target.path)
        }
    };

    // The inline fragments the chain needs on the way in. At hop `i` sits the
    // fragment enclosing everything nested inside it, because the next hop's
    // field lives on a type broader than what hop `i` returns: `Query.pets`
    // returns `Animal`, so a `Pet` under it is `pets { ... on Pet { … } }`. A
    // long chain can need one at more than one hop, which is why this is a
    // fragment per position rather than the single index it used to be.
    let mut narrows: Vec<Option<String>> = vec![None; chain.len()];
    for i in 1..chain.len() {
        let outer = chain[i - 1].base_type().unwrap_or_default();
        let inner = chain[i].parent.as_deref().unwrap_or_default();
        narrows[i - 1] = schema.narrowing(outer, inner);
    }
    // The type the innermost selection describes.
    let last = chain.len() - 1;
    let selected = match target.kind {
        // A type is selected inside the last hop that reaches it, narrowed to
        // it where the chain only gets as far as something broader.
        Kind::Object | Kind::Interface | Kind::Union => {
            let base = chain[last].base_type().unwrap_or_default();
            narrows[last] = schema.narrowing(base, &target.name);
            // A chain that arrives at one *member* of an abstract type has
            // already narrowed: the response there can only be that member, so
            // its own fields are the selection. Selecting the abstract type
            // would spread fragments for the members it can't be, which a
            // server rejects — `Query.viewer: User!` can't hold `... on Bot`.
            match narrows[last].is_none() && !base.eq_ignore_ascii_case(&target.name) {
                true => base.to_string(),
                false => target.name.clone(),
            }
        }
        // A field is selected on its own enclosing type, which the hop before
        // it already had to reach.
        _ => chain[last].base_type().unwrap_or_default().to_string(),
    };

    let operation_kind = match chain[0].kind {
        Kind::Mutation => "mutation",
        Kind::Subscription => "subscription",
        _ => "query",
    };

    // Variables first: every argument along the chain, deduped so a repeated
    // name (two `id`s) doesn't collide in the signature.
    let (vars, optional) = Variables::collect(&chain, required);

    // Then the selection, innermost outward.
    let mut deprecated = Vec::new();
    if let Some(reason) = &target.deprecated {
        deprecated.push(match reason.is_empty() {
            true => target.path.clone(),
            false => format!("{} ({})", target.path, one_line(reason)),
        });
    }
    // An input target asks "where does this go", not "what comes back", so it
    // draws the barest selection the server will accept and leaves the payload
    // to `--depth` — the eight lines of leaf fields were burying the one line
    // the draft exists to show.
    let depth = depth
        .unwrap_or(match required {
            Some(_) => 0,
            None => 1,
        })
        .min(MAX_DEPTH);
    let mut body = schema.selection(&selected, depth, &mut deprecated, &mut Vec::new());
    if body.is_empty() && !selected.is_empty() && !schema.is_leaf(&selected) {
        // A selection set is mandatory on an object return, so depth 0 takes
        // the one field that is always valid rather than emitting a parse error.
        body.push("__typename".to_string());
    }
    // A selection of markers and a `__typename` is runnable and fetches
    // nothing — every field at this level returns an object. Common enough to
    // be the first thing anyone sees: a Relay connection's fields are `info`
    // and `results`, or `edges` and `pageInfo`, and none of them is a leaf.
    // Reported rather than fixed by drafting deeper, because how deep someone
    // wants to go is exactly what this module refuses to guess.
    //
    // Only where a level was actually asked for: an input target draws depth 0
    // deliberately, and its payload's leaves are hidden rather than absent.
    //
    // And only where the flag can do something. A marker is one of two holes,
    // and `--depth` fills only the first: an object-valued field, or a field
    // that needs arguments and so can't be selected bare at any depth. A
    // namespace container is all of the second — `Mutation.card`'s ten fields
    // every one — and pointing at `--depth` there sent the reader after a flag
    // that changes nothing. That marker says "needs arguments" for itself,
    // which is why it gets no note rather than a different one.
    let no_leaves = depth > 0
        && !body.is_empty()
        && body.iter().all(|l| l == "__typename" || l.starts_with('#'))
        && body.iter().any(|l| l.ends_with(HOLE));

    for (depth, field) in chain.iter().enumerate().rev() {
        // Innermost first, so the fragment is wrapped before the field that
        // encloses it — with the `__typename` that makes the response readable
        // back.
        if let Some(on) = narrows[depth].as_ref() {
            // An abstract selection already emits its own `__typename`, and it
            // lands at the same level in the response.
            let named = body.iter().any(|l| l == "__typename");
            let mut fragment = match named {
                true => Vec::new(),
                false => vec!["__typename".to_string()],
            };
            fragment.extend(block(format!("... on {on}"), body));
            body = fragment;
        }
        let args = vars.rendered_for(depth);
        body = if body.is_empty() {
            // A leaf-returning field takes no selection set at all.
            vec![format!("{}{}", field.name, args)]
        } else {
            block(format!("{}{}", field.name, args), body)
        };
    }

    let mut operation = String::new();
    operation.push_str(operation_kind);
    operation.push(' ');
    let named_for = match target.kind {
        Kind::Object | Kind::Interface | Kind::Union => &target.name,
        _ => &chain.last().expect("a chain always has its target").name,
    };
    operation.push_str(&pascal_case(named_for));
    operation.push_str(&vars.signature());
    operation.push_str(" {\n");
    for line in &body {
        operation.push_str("  ");
        operation.push_str(line);
        operation.push('\n');
    }
    operation.push_str("}\n");

    let placeholders = vars.placeholders(&schema);
    Ok(Example {
        arguments: described_args(&chain),
        through,
        operation,
        description: target.description.clone(),
        variables: placeholders.values,
        variable_types: placeholders.named,
        enums: placeholders.enums,

        optional,
        deprecated,
        via,
        alternatives,
        no_leaves,
    })
}

/// Records indexed the two ways drafting needs: what kind a type name is, and
/// what fields a type has.
struct Schema<'a> {
    kinds: HashMap<&'a str, Kind>,
    /// The type definitions themselves, for what `kinds` can't answer —
    /// chiefly a union's members.
    types: HashMap<&'a str, &'a SchemaRecord>,
    fields: HashMap<&'a str, Vec<&'a SchemaRecord>>,
    roots: Vec<&'a SchemaRecord>,
    /// Filled by [`reach`](Schema::reach) the first time a draft asks how to
    /// get somewhere. A root target never asks, and the walk is the only part
    /// of drafting that touches the whole schema.
    reached: OnceCell<Reach<'a>>,
}

impl<'a> Schema<'a> {
    fn index(records: &'a [SchemaRecord]) -> Self {
        let mut kinds = HashMap::new();
        let mut types = HashMap::new();
        let mut fields: HashMap<&str, Vec<&SchemaRecord>> = HashMap::new();
        let mut roots = Vec::new();
        for r in records {
            match r.kind {
                Kind::Query | Kind::Mutation | Kind::Subscription => {
                    roots.push(r);
                    if let Some(p) = r.parent.as_deref() {
                        fields.entry(p).or_default().push(r);
                    }
                }
                Kind::Field | Kind::InputField | Kind::EnumValue => {
                    if let Some(p) = r.parent.as_deref() {
                        fields.entry(p).or_default().push(r);
                    }
                }
                _ => {
                    kinds.insert(r.name.as_str(), r.kind);
                    types.insert(r.name.as_str(), r);
                }
            }
        }
        Self {
            kinds,
            types,
            fields,
            roots,
            reached: OnceCell::new(),
        }
    }

    /// The JSON skeleton for one value of `type_ref`, and the enums it reaches.
    ///
    /// An input object becomes an object of its fields rather than a
    /// `"<SomeInput!>"` placeholder that only restates the variable signature —
    /// the block you paste into a client should be the one that shows the
    /// shape. A list gets one element, since a second would say nothing the
    /// first didn't.
    ///
    /// `ancestors` carries the input objects already open above this point, so
    /// a self-referential filter (`Filter { and: [Filter!] }`) closes as a
    /// `"<Filter>"` placeholder — its shape is on screen directly above, in the
    /// object that contains it.
    fn skeleton(
        &self,
        type_ref: &str,
        ancestors: &mut Vec<String>,
        enums: &mut Vec<String>,
    ) -> Value {
        let bare = type_ref.trim();
        let bare = bare.strip_suffix('!').unwrap_or(bare).trim();
        if let Some(inner) = bare.strip_prefix('[').and_then(|t| t.strip_suffix(']')) {
            return Value::Array(vec![self.skeleton(inner, ancestors, enums)]);
        }

        let placeholder = || Value::String(format!("<{}>", type_ref.trim()));
        match self.kinds.get(bare) {
            Some(Kind::Enum) => {
                let values: Vec<&str> = self
                    .fields
                    .get(bare)
                    .into_iter()
                    .flatten()
                    .map(|v| v.name.as_str())
                    .collect();
                // JSON can't hold a choice, so the values go in their own
                // block and the placeholder names the type that indexes it.
                if !values.is_empty() {
                    let line = format!("{bare} = {}", values.join(" | "));
                    if !enums.contains(&line) {
                        enums.push(line);
                    }
                }
                placeholder()
            }
            Some(Kind::InputObject)
                if !ancestors.iter().any(|a| a == bare) && ancestors.len() < MAX_NESTING =>
            {
                ancestors.push(bare.to_string());
                let mut map = Map::new();
                for f in self.fields.get(bare).into_iter().flatten() {
                    if f.kind != Kind::InputField {
                        continue;
                    }
                    let ty = f.type_ref.as_deref().unwrap_or("");
                    let mut value = self.skeleton(ty, ancestors, enums);
                    // A default is what makes even a non-null field optional,
                    // so a bare `"<OrderDirection!>"` reads as mandatory when
                    // omitting it is fine. Written as the schema writes it.
                    if let (Some(d), Value::String(p)) = (f.default.as_deref(), &mut value) {
                        *p = format!("{} = {d}>", p.trim_end_matches('>'));
                    }
                    map.insert(f.name.clone(), value);
                }
                ancestors.pop();
                // An input this schema has no fields for: `{}` would claim it
                // takes nothing, which is a stronger statement than we can
                // make. The placeholder says "fill this in" and stays honest.
                match map.is_empty() {
                    true => placeholder(),
                    false => Value::Object(map),
                }
            }
            _ => placeholder(),
        }
    }

    /// Every way to pass a value of `input`, best first: the input the
    /// argument actually carries, the path naming it, and the chain of fields
    /// an operation nests to get there.
    ///
    /// An input object is passable through the field that takes it — but one
    /// nothing takes may still be *held* by one that something takes
    /// (`AddressValidationInput { address: AddressInput! }`), and then the
    /// operations that show where it goes are the outer ones'. Nearest holders
    /// first, so what you paste is the smallest thing containing what you asked
    /// about — and *all* of them at that distance, because two inputs holding
    /// the same thing are two different answers to "where does this go" and
    /// picking one silently passes it off as the only one. A path through a
    /// holder names the holder, since the argument no longer carries the type
    /// that was asked about and the chain alone can't say which one it does.
    ///
    /// How many levels up those holders sit comes back even when the chains
    /// don't, because "buried too deep to show" and "nothing takes it anywhere"
    /// are different news for the caller — the same reason
    /// [`chains_reaching`](Self::chains_reaching) reports its hops.
    fn chains_passing(&self, input: &'a str) -> (Option<usize>, Vec<Passing<'a>>) {
        let mut seen: HashSet<&str> = [input].into_iter().collect();
        let mut frontier = vec![input];
        let mut levels = 0;
        while !frontier.is_empty() {
            let mut chains: Vec<Passing<'a>> = frontier
                .iter()
                .flat_map(|&held| {
                    self.chains_taking(held)
                        .into_iter()
                        .map(move |(arg, chain)| {
                            let path = match held == input {
                                true => format!("{}({arg}:)", label(&chain)),
                                false => format!("{}({arg}: {held})", label(&chain)),
                            };
                            Passing { held, path, chain }
                        })
                })
                .collect();
            if !chains.is_empty() {
                if levels > MAX_NESTING {
                    return (Some(levels), Vec::new());
                }
                chains.sort_by(|a, b| {
                    chain_order(&a.chain)
                        .cmp(&chain_order(&b.chain))
                        .then(a.path.cmp(&b.path))
                });
                return (Some(levels), chains);
            }
            frontier = frontier
                .iter()
                .flat_map(|&at| self.inputs_holding(at))
                .filter(|holder| seen.insert(holder))
                .collect();
            frontier.sort_unstable();
            levels += 1;
        }
        (None, Vec::new())
    }

    /// The input objects with a field of type `input` — the way *in* to it.
    fn inputs_holding(&self, input: &str) -> Vec<&'a str> {
        let mut holders: Vec<&str> = self
            .fields
            .values()
            .flatten()
            .filter(|r| r.kind == Kind::InputField && r.base_type() == Some(input))
            .filter_map(|r| r.parent.as_deref())
            .collect();
        holders.sort_unstable();
        holders.dedup();
        holders
    }

    /// Every field taking an argument of type `input`, each paired with the
    /// argument's name and the chain of fields an operation nests to reach it.
    /// Unordered — [`chains_passing`](Self::chains_passing) sorts, because the
    /// paths from several holders rank against each other.
    ///
    /// A root consumer is the whole chain by itself. A consumer on a plain
    /// object is reached the way any nested field is — through the chain
    /// leading to the type it hangs off — and is dropped when nothing leads
    /// there, since an alternative you can't call isn't one.
    fn chains_taking(&self, input: &str) -> Vec<(&'a str, Vec<&'a SchemaRecord>)> {
        let mut chains: Vec<(&'a str, Vec<&'a SchemaRecord>)> = Vec::new();
        // Every record with arguments hangs off some parent, so this covers the
        // roots and the object fields both.
        for r in self.fields.values().flatten().copied() {
            for (arg, ty) in r.arg_types() {
                if ty != input {
                    continue;
                }
                let mut chain = match r.kind {
                    Kind::Query | Kind::Mutation | Kind::Subscription => Vec::new(),
                    Kind::Field => {
                        let Some(parent) = r.parent.as_deref() else {
                            continue;
                        };
                        match self.chains_reaching(parent, false).1.into_iter().next() {
                            Some(prefix) => prefix,
                            None => continue,
                        }
                    }
                    _ => continue,
                };
                chain.push(r);
                // The argument is carried alongside, not just the path: it's
                // half the answer to "where does this input go".
                chains.push((arg, chain));
            }
        }
        chains
    }

    /// Every shortest chain of fields from a root that reaches `type_name`,
    /// best first, paired with how many hops that took.
    ///
    /// "Reaches" is whatever [`narrowing`](Self::narrowing) can then make
    /// selectable: a field returning the type itself, or one returning
    /// something whose runtime types overlap it — `Query.pets: [Animal!]!`
    /// reaches `Pet.nickname` as `pets { ... on Pet { nickname } }`, and
    /// calling a field you can query unreachable is the worse answer. An exact
    /// return beats a narrowing at the same distance but never across
    /// distances: a shorter path is the friendlier draft however it gets there.
    ///
    /// The hop count comes back even when the chains don't, because "eight hops
    /// away" and "not there at all" are different news for the caller.
    fn chains_reaching(
        &self,
        type_name: &str,
        whole: bool,
    ) -> (Option<usize>, Vec<Vec<&'a SchemaRecord>>) {
        let reach = self.reach();
        let wanted = self.possible(type_name);
        // (hops, 0 for the type itself and 1 for something it narrows from),
        // so the plain minimum is the pick and its ties are the alternatives.
        let mut goals: Vec<(usize, usize, &str)> = Vec::new();
        for (&at, &hops) in &reach.hops {
            // Zero hops is a root type: where a walk starts, not somewhere a
            // field arrives, so there's no chain to draft.
            if hops == 0 {
                continue;
            }
            // 0: the type itself. 1: something broader, which a fragment
            // narrows to it. 2: one of its members — already narrower, so when
            // the *type* is what was asked for the draft can only answer a
            // smaller question, and it loses to either of the above at the same
            // distance. A field on the type is on that member too, so reaching
            // it there costs nothing and the penalty doesn't apply.
            let member = if whole { 2 } else { 1 };
            let tier = if at.eq_ignore_ascii_case(type_name) {
                0
            } else if wanted.contains(&at) {
                member
            } else if self.possible(at).iter().any(|m| wanted.contains(m)) {
                1
            } else {
                continue;
            };
            goals.push((hops, tier, at));
        }
        let Some(&(hops, tier, _)) = goals.iter().min() else {
            return (None, Vec::new());
        };
        if hops > MAX_HOPS {
            return (Some(hops), Vec::new());
        }
        let mut chains: Vec<Vec<&'a SchemaRecord>> = goals
            .iter()
            .filter(|g| (g.0, g.1) == (hops, tier))
            .flat_map(|&(_, _, at)| reach.edges.get(at).into_iter().flatten())
            .map(|&(from, field)| {
                let mut chain = reach.chain_to(from);
                chain.push(field);
                chain
            })
            .collect();
        chains.sort_by_key(|c| chain_order(c));
        (Some(hops), chains)
    }

    /// The type graph as seen from the root operation fields, walked at most
    /// once per draft and shared by both edges: reaching a nested field and
    /// reaching the field that *takes* an input are the same walk.
    fn reach(&self) -> &Reach<'a> {
        self.reached.get_or_init(|| self.walk())
    }

    /// One breadth-first walk out from the root operation fields.
    ///
    /// Breadth-first is what makes a cyclic type graph safe without tracking a
    /// path per node: a type is settled the first time it's seen, so
    /// `User.posts: [Post!]!` beside `Post.author: User!` closes instead of
    /// looping, every chain is a shortest one, and no chain passes through a
    /// type twice.
    fn walk(&self) -> Reach<'a> {
        let mut hops: HashMap<&'a str, usize> = HashMap::new();
        let mut edges: HashMap<&'a str, Vec<(&'a str, &'a SchemaRecord)>> = HashMap::new();
        let mut frontier: Vec<&'a str> = Vec::new();
        for r in &self.roots {
            if let Some(p) = r.parent.as_deref() {
                if hops.insert(p, 0).is_none() {
                    frontier.push(p);
                }
            }
        }
        frontier.sort_unstable();
        let mut depth = 0;
        while !frontier.is_empty() {
            depth += 1;
            let mut next: Vec<&'a str> = Vec::new();
            for &from in &frontier {
                for field in self.outgoing(from) {
                    let Some(base) = field.base_type() else {
                        continue;
                    };
                    // A scalar or enum return is the end of the road: nothing
                    // to select underneath it, so nothing further to reach.
                    if self.is_leaf(base) {
                        continue;
                    }
                    match hops.get(base).copied() {
                        // Settled nearer already; a longer way in isn't a way.
                        Some(seen) if seen < depth => continue,
                        Some(_) => {}
                        None => {
                            hops.insert(base, depth);
                            next.push(base);
                        }
                    }
                    edges.entry(base).or_default().push((from, field));
                }
            }
            next.sort_unstable();
            frontier = next;
        }
        // Friendliest hop first, so rebuilding a chain asks least of the caller
        // at every level of it.
        for into in edges.values_mut() {
            into.sort_by_key(|(_, field)| root_order(field));
        }
        Reach { hops, edges }
    }

    /// The fields selectable one hop out from `type_name`: its own, plus what
    /// an abstract type's members add, which an inline fragment reaches. A
    /// member's redeclaration of a field the abstract type already has is
    /// skipped — it's the same field, and reachable without the fragment.
    fn outgoing(&self, type_name: &str) -> Vec<&'a SchemaRecord> {
        let own: Vec<&'a SchemaRecord> = self
            .fields
            .get(type_name)
            .into_iter()
            .flatten()
            .copied()
            .filter(|f| {
                matches!(
                    f.kind,
                    Kind::Field | Kind::Query | Kind::Mutation | Kind::Subscription
                )
            })
            .collect();
        let mut out = own.clone();
        for member in self.possible(type_name) {
            if member == type_name {
                continue;
            }
            let added = self
                .fields
                .get(member)
                .into_iter()
                .flatten()
                .copied()
                .filter(|f| f.kind == Kind::Field && !own.iter().any(|o| o.name == f.name));
            out.extend(added);
        }
        out
    }

    /// What a value of `type_name` can be at runtime, by name.
    fn possible(&self, type_name: &str) -> Vec<&'a str> {
        self.types
            .get(type_name)
            .copied()
            .map(SchemaRecord::runtime_types)
            .unwrap_or_default()
    }

    /// Field names that mean different shapes in different members of `rec` —
    /// `email: String` on one and `email: String!` on another. Selecting both
    /// under one response name is invalid however the server feels about it,
    /// so these are the names that have to be aliased apart.
    fn clashing_names(&self, rec: &SchemaRecord, max: usize) -> HashSet<&'a str> {
        let mut seen: HashMap<&str, &str> = HashMap::new();
        let mut clashing = HashSet::new();
        for member in rec.possible_types.iter().take(max) {
            for f in self.fields.get(member.as_str()).into_iter().flatten() {
                let ty = f.type_ref.as_deref().unwrap_or_default();
                if f.kind == Kind::Field && seen.insert(&f.name, ty).is_some_and(|old| old != ty) {
                    clashing.insert(f.name.as_str());
                }
            }
        }
        clashing
    }

    /// The inline fragment needed to select `wanted`'s fields inside a field
    /// returning `base`. `None` when they're already selectable there — an
    /// object implementing the interface carries its fields itself.
    fn narrowing(&self, base: &str, wanted: &str) -> Option<String> {
        let selectable = base.eq_ignore_ascii_case(wanted) || self.possible(wanted).contains(&base);
        (!selectable).then(|| wanted.to_string())
    }

    /// Whether a type needs no selection set — a scalar, an enum, or a name
    /// the schema never defines (the built-in scalars, which SDL omits).
    fn is_leaf(&self, type_name: &str) -> bool {
        !matches!(
            self.kinds.get(type_name),
            Some(Kind::Object | Kind::Interface | Kind::Union | Kind::InputObject)
        )
    }

    /// The depth to expand `f` at, or `None` to leave it as a marker.
    ///
    /// Two ways in. A level of `depth` buys the ordinary one, and spending it
    /// is what guarantees the recursion ends. The payload/errors convention
    /// gets in free at the last level — a mutation drafted without its errors
    /// block reads wrong — and pays for that by not re-opening a type already
    /// being selected above it, which would be a cycle with nothing left to
    /// spend. Every recursion therefore either shortens `depth` or lengthens
    /// `open` with a type not already in it, and both are bounded.
    fn expands(
        &self,
        f: &SchemaRecord,
        base: &str,
        depth: usize,
        open: &[String],
    ) -> Option<usize> {
        if depth > 1 {
            return Some(depth - 1);
        }
        let convention =
            f.name.eq_ignore_ascii_case("errors") && !open.iter().any(|open| open == base);
        convention.then_some(depth)
    }

    /// The selection set for `type_name`: its leaf fields, plus a marker for
    /// each object-valued field so the hole is visible. Empty for a leaf type.
    ///
    /// `depth` is the termination measure and every recursion below spends a
    /// level of it — except the payload/errors convention, which expands at the
    /// last level too. `open` is what bounds that one: the types already being
    /// selected above this point, carried the way [`skeleton`](Self::skeleton)
    /// carries `ancestors`. A free level into a type already open is what made
    /// `Payload { errors: Payload }` recurse until the stack ran out.
    fn selection(
        &self,
        type_name: &str,
        depth: usize,
        deprecated: &mut Vec<String>,
        open: &mut Vec<String>,
    ) -> Vec<String> {
        if type_name.is_empty() || self.is_leaf(type_name) || depth == 0 {
            return Vec::new();
        }
        open.push(type_name.to_string());
        let lines = self.selection_of(type_name, depth, deprecated, open);
        open.pop();
        lines
    }

    /// [`selection`](Self::selection)'s body, with `type_name` already on
    /// `open` — split out so every exit pops it exactly once.
    fn selection_of(
        &self,
        type_name: &str,
        depth: usize,
        deprecated: &mut Vec<String>,
        open: &mut Vec<String>,
    ) -> Vec<String> {
        // An abstract type has no fields of its own to select — a union never,
        // an interface only the common ones — so what the caller actually wants
        // is spelled with inline fragments over the concrete types.
        if let Some(rec) = self.types.get(type_name) {
            if rec.kind == Kind::Union {
                return self.inline_fragments(rec, depth, deprecated, &[], open);
            }
        }

        let mut lines = Vec::new();
        let mut deferred = Vec::new();
        for f in self.fields.get(type_name).into_iter().flatten() {
            if f.kind != Kind::Field {
                continue;
            }
            let Some(base) = f.base_type() else { continue };
            // A field with required arguments can't be selected bare.
            if f.args.iter().any(|a| a.trim_end().ends_with('!')) {
                deferred.push(format!("# {}: {} — needs arguments", f.name, base));
                continue;
            }
            let note = match &f.deprecated {
                // Flagged, not dropped (see the module doc). The caller warns too.
                Some(reason) if reason.is_empty() => {
                    deprecated.push(f.path.clone());
                    "  # deprecated".to_string()
                }
                Some(reason) => {
                    deprecated.push(f.path.clone());
                    format!("  # deprecated: {}", one_line(reason))
                }
                None => String::new(),
            };
            if self.is_leaf(base) {
                lines.push(format!("{}{note}", f.name));
            } else if let Some(inner_depth) = self.expands(f, base, depth, open) {
                let inner = self.selection(base, inner_depth, deprecated, open);
                // After the brace, never before it: a note is a `#` comment,
                // and everything past it on the line — the `{` included — is
                // comment too, which leaves the document unbalanced.
                lines.push(format!("{} {{{note}", f.name));
                lines.extend(inner.into_iter().map(|l| format!("  {l}")));
                lines.push("}".to_string());
            } else {
                // `{ … }`, not `...`: inside a selection set that would read as
                // a fragment spread. Commented because there's no valid empty
                // selection set (see the module doc).
                deferred.push(format!("# {}: {} {HOLE}", f.name, base));
            }
        }
        // An interface's own fields are only the common ones. Its implementors
        // usually carry the fields you actually came for, and they're
        // unreachable without fragments — so append what each one adds.
        let fragments = match self.types.get(type_name) {
            Some(rec) if rec.kind == Kind::Interface => {
                let common: Vec<&str> = self
                    .fields
                    .get(type_name)
                    .into_iter()
                    .flatten()
                    .map(|f| f.name.as_str())
                    .collect();
                self.inline_fragments(rec, depth, deprecated, &common, open)
            }
            _ => Vec::new(),
        };

        // Measured before the markers are appended, because a marker is a
        // comment: a type whose fields are *all* object-valued yields a
        // selection set holding nothing but comments, which no server will
        // parse. `__typename` is always valid and keeps the query runnable —
        // it's equally the answer for a type this schema doesn't detail.
        if lines.is_empty() && fragments.is_empty() {
            lines.push("__typename".to_string());
        }
        lines.extend(deferred);
        lines.extend(fragments);
        lines
    }

    /// `... on User { … }` for each of an abstract type's concrete types.
    /// `skip` names fields already selected on the abstract type itself, so an
    /// interface's fragments show only what each implementor adds.
    fn inline_fragments(
        &self,
        rec: &SchemaRecord,
        depth: usize,
        deprecated: &mut Vec<String>,
        skip: &[&str],
        open: &mut Vec<String>,
    ) -> Vec<String> {
        /// Enough to show the shape without burying the query; a big union
        /// lists the rest as a comment instead.
        const MAX_MEMBERS: usize = 6;

        if rec.possible_types.is_empty() {
            // A union we can't detail is the only case with nothing to select.
            return match rec.kind {
                Kind::Union => vec![
                    "__typename".to_string(),
                    "# add inline fragments: ... on ConcreteType { … }".to_string(),
                ],
                _ => Vec::new(),
            };
        }
        // Two members selecting the same name with different types are a
        // response shape the spec forbids (§5.3.2) — `email: String` beside
        // `email: String!`. An alias per member keeps both fields and keeps the
        // draft pasteable; without one a conformant server rejects the whole
        // operation.
        let clashing = self.clashing_names(rec, MAX_MEMBERS);
        let mut fragments = Vec::new();
        for member in rec.possible_types.iter().take(MAX_MEMBERS) {
            // Drop what the abstract type already selected — and drop it
            // whole. A field with a selection set spans several lines, so
            // dropping its opening line alone left the body and the closing
            // brace behind, and the operation didn't parse. Only the outermost
            // block's closer is unindented at this level, which is what ends
            // the drop.
            //
            // Dropping every marker instead, as this once did, dropped whole
            // implementors: one whose additions are all object-valued has
            // nothing but markers, and vanished with nothing saying it exists.
            let mut inner: Vec<String> = Vec::new();
            let mut dropping = false;
            for line in self.selection(member, depth, deprecated, open) {
                if dropping {
                    dropping = line != "}";
                    continue;
                }
                let named = line.strip_prefix("# ").unwrap_or(&line);
                let name = named.split([' ', '{', ':']).next().unwrap_or(named);
                if skip.contains(&name) {
                    // Measured on the code, not the whole line: a deprecated
                    // field carries its note past the brace.
                    let code = line.split('#').next().unwrap_or_default();
                    dropping = code.trim_end().ends_with('{');
                    continue;
                }
                inner.push(line);
            }
            if inner.is_empty() {
                continue; // this implementor adds nothing of its own
            }
            // A marker is a comment, so markers alone are an empty selection
            // set, which no server parses.
            if inner.iter().all(|l| l.starts_with('#')) {
                inner.insert(0, "__typename".to_string());
            }
            // Only this fragment's own level: a nested line is indented, and
            // belongs to a response object of its own.
            for line in inner.iter_mut() {
                let name = line.split([' ', '{', ':']).next().unwrap_or(line);
                if !line.starts_with(' ') && clashing.contains(name) {
                    *line = format!("{}{}: {line}", uncapitalise(member), pascal_case(name));
                }
            }
            fragments.push(format!("... on {member} {{"));
            fragments.extend(inner.into_iter().map(|l| format!("  {l}")));
            fragments.push("}".to_string());
        }
        if fragments.is_empty() {
            return Vec::new();
        }
        // `__typename` is what makes the fragments interpretable in a response.
        let mut lines = vec!["__typename".to_string()];
        lines.append(&mut fragments);
        if rec.possible_types.len() > MAX_MEMBERS {
            lines.push(format!(
                "# {} more: {}",
                rec.possible_types.len() - MAX_MEMBERS,
                rec.possible_types[MAX_MEMBERS..].join(", ")
            ));
        }
        lines
    }
}

/// One way to pass an input: which input the argument actually carries, the
/// path that names it, and the chain of fields an operation nests to get there.
struct Passing<'a> {
    /// The input the argument takes — the one asked about, or the holder it
    /// rides inside.
    held: &'a str,
    path: String,
    chain: Vec<&'a SchemaRecord>,
}

/// What the type graph looks like from the root operation fields: how far
/// each type is, and every field that gets there on the last hop.
struct Reach<'a> {
    /// Hops from a root operation field. Zero for the root types themselves.
    hops: HashMap<&'a str, usize>,
    /// Per type, the fields reaching it in exactly `hops` hops, each paired
    /// with the type it was selected on — the field's own parent, except where
    /// the chain got there through an abstract type and the field belongs to
    /// one of its members. Friendliest first.
    edges: HashMap<&'a str, Vec<(&'a str, &'a SchemaRecord)>>,
}

impl<'a> Reach<'a> {
    /// The chain of fields from a root to `type_name`, outermost first, taking
    /// the friendliest edge at every hop. Empty for a root type. Terminates
    /// because an edge into a type always comes from one hop nearer.
    fn chain_to(&self, type_name: &str) -> Vec<&'a SchemaRecord> {
        let mut chain = Vec::new();
        let mut at = type_name;
        while self.hops.get(at).is_some_and(|&h| h > 0) {
            let Some(&(from, field)) = self.edges.get(at).and_then(|e| e.first()) else {
                break;
            };
            chain.push(field);
            at = from;
        }
        chain.reverse();
        chain
    }
}

/// The variables block, ready to render.
struct Placeholders {
    values: Value,
    /// `filter: PostFilter` for each variable expanded into an object or list,
    /// whose JSON key alone no longer names its type.
    named: Vec<String>,
    /// Enum types the skeleton reached, `Role = ADMIN | MEMBER`. JSON has no
    /// way to hold "one of these", so the choice is listed alongside.
    enums: Vec<String>,
}

/// The operation's variables: one per argument along the field chain.
struct Variables {
    /// `(depth, arg name, variable name, type)`
    entries: Vec<(usize, String, String, String)>,
}

impl Variables {
    /// Split the chain's arguments: the ones a caller must supply become
    /// variables, the rest are returned as notes.
    ///
    /// `required` names an input type whose arguments are supplied whatever the
    /// schema says about them — the draft exists to show where that input goes,
    /// and one that quietly omits it because the field tolerates its absence
    /// answers nothing.
    fn collect(chain: &[&SchemaRecord], required: Option<&str>) -> (Self, Vec<String>) {
        let mut entries: Vec<(usize, String, String, String)> = Vec::new();
        let mut optional = Vec::new();
        for (depth, field) in chain.iter().enumerate() {
            for arg in &field.args {
                let Arg {
                    name,
                    type_ref,
                    default,
                } = split_arg(arg);
                // A default means the server fills it in, so even a non-null
                // argument needs nothing from the caller.
                let demanded = required.is_some_and(|t| base_of(type_ref) == t);
                if !demanded && (!type_ref.ends_with('!') || default.is_some()) {
                    // A signature carries a default, and a default can be a
                    // block string; it's rendered as a comment like the rest.
                    optional.push(format!("{}({})", field.name, one_line(arg)));
                    continue;
                }
                // Disambiguate a name already taken by an outer field's arg.
                // A long chain can repeat the field name too — two `node(id:)`
                // hops both want `nodeId` — so the qualified form is numbered
                // until it's free rather than colliding in the signature.
                let mut var = name.to_string();
                if entries.iter().any(|(_, _, v, _)| *v == var) {
                    let qualified = format!("{}{}", field.name, pascal_case(name));
                    var = qualified.clone();
                    let mut n = 2;
                    while entries.iter().any(|(_, _, v, _)| *v == var) {
                        var = format!("{qualified}{n}");
                        n += 1;
                    }
                }
                entries.push((depth, name.to_string(), var, type_ref.to_string()));
            }
        }
        (Self { entries }, optional)
    }

    /// `($id: ID!, $first: Int)`, or empty when there are no arguments.
    fn signature(&self) -> String {
        if self.entries.is_empty() {
            return String::new();
        }
        let inner: Vec<String> = self
            .entries
            .iter()
            .map(|(_, _, var, ty)| format!("${var}: {ty}"))
            .collect();
        format!("({})", inner.join(", "))
    }

    /// `(id: $id, first: $first)` for the field at `depth`, or empty.
    fn rendered_for(&self, depth: usize) -> String {
        let inner: Vec<String> = self
            .entries
            .iter()
            .filter(|(d, _, _, _)| *d == depth)
            .map(|(_, name, var, _)| format!("{name}: ${var}"))
            .collect();
        if inner.is_empty() {
            String::new()
        } else {
            format!("({})", inner.join(", "))
        }
    }

    /// The variables block: JSON to paste, plus the two things it can't say.
    ///
    /// A scalar placeholder names its type (`"<ID!>"`) — unmistakably a blank
    /// to fill rather than a usable value. An input object is expanded into its
    /// fields instead, so the thing you paste is the thing that shows the shape.
    fn placeholders(&self, schema: &Schema) -> Placeholders {
        let mut map = Map::new();
        let mut enums = Vec::new();
        let mut named = Vec::new();
        for (_, _, var, ty) in &self.entries {
            let value = schema.skeleton(ty, &mut Vec::new(), &mut enums);
            // Expanding costs the reader the type name: `"filter": { … }` no
            // longer says it's a PostFilter, and the signature that does say so
            // is twenty lines up.
            if value.is_object() || value.is_array() {
                named.push(format!("{var}: {ty}"));
            }
            map.insert(var.clone(), value);
        }
        Placeholders {
            values: Value::Object(map),
            named,
            enums,
        }
    }
}

/// How far from a root a draft will chase a target.
///
/// Namespaced roots (`Query.payroll: PayrollQueries`, with the real fields
/// hanging off it) put ordinary targets three and four hops out, so a cap that
/// clears only the direct case refuses most of a schema. Six is measured
/// rather than picked: in both production schemas gqls is swept against, every
/// type any chain reaches at all lands within six hops, so the cap never fires
/// there. It guards a schema shaped worse than those, where a draft that deep
/// would be more nesting than help — and the refusal names the distance, so
/// "too deep" reads differently from "not there".
const MAX_HOPS: usize = 6;

/// How deep the variable skeleton expands an input object. Deep enough for any
/// input anyone hand-writes; a guard, not a policy. Past it the placeholder
/// stands, and `gqls <Type>` lists the fields.
///
/// It doubles as the cap on the holder walk, because the two distances are the
/// same one: an input held this many levels inside the one being passed is the
/// deepest the skeleton will still name. Any deeper and a draft announcing
/// "X is passed inside Y" would print variables that never mention X.
const MAX_NESTING: usize = 6;

/// How an object-valued marker ends — the one hole `--depth` fills. Stated
/// once because two places depend on it: the marker is written with it, and
/// whether a draft is worth pointing at `--depth` is read back off it.
const HOLE: &str = "{ … }";

/// The ceiling on `--depth`. A selection set fans out by the branching factor
/// of the schema, so the draft grows geometrically: on GitHub's schema
/// `Repository` drafts 6KB at depth 1, 1.8MB at 4 and 877MB at 7, and on
/// chime's `User` 1.6KB at 1 and 90MB at 12. Nothing past this is a document
/// anyone can paste, and a mistyped `--depth` used to mean a stack overflow or
/// a gigabyte of stdout. A guard on the typo, not an opinion about the draft —
/// which is why it's this far out rather than at the handful of levels anyone
/// reads.
pub const MAX_DEPTH: usize = 6;

/// A chain as one readable path: `Query.early_pay > EarlyPayQueryRoot.status`.
/// One form in text and in `--json` both, since a path is several fields now
/// and naming only its first says almost nothing.
fn label(chain: &[&SchemaRecord]) -> String {
    chain
        .iter()
        .map(|r| r.path.as_str())
        .collect::<Vec<_>>()
        .join(" > ")
}

/// Chains ordered so the friendliest path drafts first: shortest, then the
/// fewest arguments to fill in, then stably by name. Past the length that is
/// [`root_order`], which is what a single hop has always been sorted by.
fn chain_order(chain: &[&SchemaRecord]) -> (usize, usize, usize, String) {
    let path = label(chain);
    (
        chain.len(),
        chain.iter().map(|f| required_args(f)).sum(),
        path.len(),
        path,
    )
}

/// Why a target can't be drafted through: too deep to be worth it, or not
/// there at all. Different news — one says trim the question, the other says
/// ask a different one — so they don't share a sentence.
fn out_of_reach(type_name: &str, hops: Option<usize>) -> String {
    let close = format!("Try `gqls --returns {type_name}` to see what's close.");
    match hops {
        Some(hops) => format!(
            "is {hops} hops from a root field, past the {MAX_HOPS}-hop cap — that much \
             nesting is more query than help. {close}"
        ),
        None => format!(
            "isn't reachable from a root field — nothing returns {type_name}, or \
             anything it narrows from. {close}"
        ),
    }
}

/// Why an input can't be drafted: nothing on the way out of it is ever taken,
/// or the nearest thing that is holds it deeper than the variables block
/// reaches. Different news — one says nothing passes this, the other says a
/// draft naming it would print variables that never mention it — so they don't
/// share a sentence.
fn unpassable(input: &str, levels: Option<usize>) -> String {
    let refer = format!("Try `gqls {input}` to see what references it.");
    match levels {
        Some(levels) => format!(
            "{input} sits {levels} levels inside the nearest input anything takes, past \
             the {MAX_NESTING}-level cap — the variables block wouldn't reach it, so a \
             draft would show where it goes without ever showing it. {refer}"
        ),
        None => format!(
            "nothing takes an argument of type {input}, and no input that holds one is \
             taken either, so there's no operation to draft. {refer}"
        ),
    }
}

/// A selection set: `header { … }`, with `body` indented one level inside.
fn block(header: String, body: Vec<String>) -> Vec<String> {
    let mut lines = vec![format!("{header} {{")];
    lines.extend(body.into_iter().map(|l| format!("  {l}")));
    lines.push("}".to_string());
    lines
}

/// What each documented argument along `chain` is for, in the order the
/// operation takes them. Undocumented arguments are left out entirely: a block
/// of names with nothing beside them says less than no block at all.
///
/// A name two hops both use is qualified with its field, since one word can't
/// stand for two different arguments in the same list.
fn described_args(chain: &[&SchemaRecord]) -> Vec<ArgDoc> {
    let mut times: HashMap<&str, usize> = HashMap::new();
    for field in chain {
        for arg in &field.args {
            *times.entry(split_arg(arg).name).or_default() += 1;
        }
    }
    let mut out = Vec::new();
    for field in chain {
        for arg in &field.args {
            let name = split_arg(arg).name;
            let Some(doc) = field.arg_descriptions.get(name) else {
                continue;
            };
            let label = match times.get(name) {
                Some(&n) if n > 1 => format!("{}({name}:)", field.name),
                _ => name.to_string(),
            };
            out.push(ArgDoc {
                name: label,
                description: doc.clone(),
            });
        }
    }
    out
}

/// Roots ordered so the friendliest entry point drafts first.
fn root_order(r: &&SchemaRecord) -> (usize, usize, String) {
    (required_args(r), r.path.len(), r.path.clone())
}

/// How many of a field's arguments are non-null, and so must be supplied.
fn required_args(r: &SchemaRecord) -> usize {
    r.args
        .iter()
        .map(|a| split_arg(a))
        .filter(|a| a.type_ref.ends_with('!') && a.default.is_none())
        .count()
}

/// Schema prose, flattened onto one line so it can sit in a `#` comment.
///
/// A comment runs to the next line terminator, so any schema text that reaches
/// a draft has to arrive without one: a deprecation reason spanning two lines
/// puts its tail into the selection set, where a server reads it as fields. The
/// same reason a note goes *after* an opening brace and never before it.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `Organization` → `organization`, for the head of an alias.
fn uncapitalise(name: &str) -> String {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) => c.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// `updateEmployee` → `UpdateEmployee`, for the operation name.
fn pascal_case(name: &str) -> String {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(
        path: &str,
        name: &str,
        kind: Kind,
        parent: Option<&str>,
        type_ref: Option<&str>,
        args: &[&str],
    ) -> SchemaRecord {
        SchemaRecord {
            path: path.into(),
            name: name.into(),
            kind,
            parent: parent.map(Into::into),
            type_ref: type_ref.map(Into::into),
            args: args.iter().map(|a| a.to_string()).collect(),
            arg_descriptions: Default::default(),
            description: None,
            deprecated: None,
            directives: vec![],
            default: None,
            possible_types: vec![],
        }
    }

    /// Query.user(id) -> User { id name role posts(Post) }, plus a mutation
    /// whose payload carries an errors block.
    fn schema() -> Vec<SchemaRecord> {
        vec![
            rec("Query", "Query", Kind::Object, None, None, &[]),
            rec("User", "User", Kind::Object, None, None, &[]),
            rec("Post", "Post", Kind::Object, None, None, &[]),
            rec("Role", "Role", Kind::Enum, None, None, &[]),
            rec("Payload", "Payload", Kind::Object, None, None, &[]),
            rec("UserError", "UserError", Kind::Object, None, None, &[]),
            rec("Input", "Input", Kind::InputObject, None, None, &[]),
            rec(
                "Query.user",
                "user",
                Kind::Query,
                Some("Query"),
                Some("User"),
                &["id: ID!"],
            ),
            rec(
                "Query.count",
                "count",
                Kind::Query,
                Some("Query"),
                Some("Int!"),
                &[],
            ),
            rec("User.id", "id", Kind::Field, Some("User"), Some("ID!"), &[]),
            rec(
                "User.name",
                "name",
                Kind::Field,
                Some("User"),
                Some("String"),
                &[],
            ),
            rec(
                "User.role",
                "role",
                Kind::Field,
                Some("User"),
                Some("Role!"),
                &[],
            ),
            rec(
                "User.posts",
                "posts",
                Kind::Field,
                Some("User"),
                Some("[Post!]!"),
                &[],
            ),
            rec(
                "User.avatar",
                "avatar",
                Kind::Field,
                Some("User"),
                Some("String"),
                &["size: Int!"],
            ),
            rec(
                "Mutation.save",
                "save",
                Kind::Mutation,
                Some("Mutation"),
                Some("Payload!"),
                &["input: Input!", "dryRun: Boolean"],
            ),
            rec(
                "Payload.ok",
                "ok",
                Kind::Field,
                Some("Payload"),
                Some("Boolean!"),
                &[],
            ),
            rec(
                "Payload.errors",
                "errors",
                Kind::Field,
                Some("Payload"),
                Some("[UserError!]!"),
                &[],
            ),
            rec(
                "UserError.message",
                "message",
                Kind::Field,
                Some("UserError"),
                Some("String!"),
                &[],
            ),
            rec(
                "Role.ADMIN",
                "ADMIN",
                Kind::EnumValue,
                Some("Role"),
                None,
                &[],
            ),
        ]
    }

    fn build_for(path: &str) -> Example {
        let records = schema();
        let target = records.iter().find(|r| r.path == path).unwrap();
        build(target, &records, Some(1)).unwrap()
    }

    #[test]
    fn root_field_becomes_a_parameterized_query() {
        let ex = build_for("Query.user");
        assert_eq!(
            ex.operation,
            "query User($id: ID!) {\n  \
               user(id: $id) {\n    \
                 id\n    \
                 name\n    \
                 role\n    \
                 # posts: Post { … }\n    \
                 # avatar: String — needs arguments\n  \
               }\n\
             }\n"
        );
        assert_eq!(ex.variables, serde_json::json!({ "id": "<ID!>" }));
    }

    #[test]
    fn a_scalar_return_gets_no_selection_set() {
        let ex = build_for("Query.count");
        assert_eq!(ex.operation, "query Count {\n  count\n}\n");
        assert_eq!(ex.variables, serde_json::json!({}));
    }

    #[test]
    fn mutation_expands_a_real_errors_block() {
        let ex = build_for("Mutation.save");
        assert!(
            ex.operation
                .starts_with("mutation Save($input: Input!) {\n  save(input: $input) {"),
            "{}",
            ex.operation
        );
        // the nullable arg is left out of the operation, but still surfaced
        assert_eq!(ex.optional, ["save(dryRun: Boolean)"]);
        assert!(
            ex.operation.contains("errors {\n      message\n    }"),
            "{}",
            ex.operation
        );
        // only what the caller must supply, typed so it can't be mistaken
        // for a usable value
        assert_eq!(ex.variables, serde_json::json!({ "input": "<Input!>" }));
    }

    #[test]
    fn nested_field_is_wrapped_in_a_root_that_returns_its_type() {
        let ex = build_for("User.posts");
        // User.posts isn't callable directly; Query.user returns a User
        // Post has no fields in this fixture, so the selection falls back to
        // __typename — always valid, and it keeps the query runnable.
        assert_eq!(
            ex.operation,
            "query Posts($id: ID!) {\n  \
               user(id: $id) {\n    \
                 posts {\n      \
                   __typename\n    \
                 }\n  \
               }\n\
             }\n"
        );
    }

    #[test]
    fn an_unreachable_field_is_an_error_not_a_guess() {
        let mut records = schema();
        // Nothing returns an Orphan at any distance, which is what unreachable
        // means now: `UserError` used to stand for this and no longer can, since
        // `Mutation.save > Payload.errors` reaches it in two.
        records.push(rec("Orphan", "Orphan", Kind::Object, None, None, &[]));
        records.push(rec(
            "Orphan.note",
            "note",
            Kind::Field,
            Some("Orphan"),
            Some("String"),
            &[],
        ));
        let target = records.pop().expect("just pushed");
        let err = build(&target, &records, Some(1)).unwrap_err().to_string();
        assert!(err.contains("isn't reachable from a root field"), "{err}");
    }

    #[test]
    fn ambiguous_roots_are_reported_rather_than_hidden() {
        let mut records = schema();
        records.push(rec(
            "Query.viewer",
            "viewer",
            Kind::Query,
            Some("Query"),
            Some("User"),
            &[],
        ));
        let target = records.iter().find(|r| r.path == "User.name").unwrap();
        let ex = build(target, &records, Some(1)).unwrap();
        // both Query.user and Query.viewer return User
        assert_eq!(ex.alternatives.len(), 1);
    }

    #[test]
    fn a_defaulted_argument_is_omitted_even_when_non_null() {
        let records = vec![
            rec("Query", "Query", Kind::Object, None, None, &[]),
            rec(
                "Query.feed",
                "feed",
                Kind::Query,
                Some("Query"),
                Some("Int!"),
                // non-null, but the schema supplies a default — nothing is
                // required of the caller
                &["first: Int! = 10", "after: String"],
            ),
        ];
        let target = records.iter().find(|r| r.path == "Query.feed").unwrap();
        let ex = build(target, &records, Some(1)).unwrap();
        assert_eq!(ex.operation, "query Feed {\n  feed\n}\n");
        assert_eq!(ex.variables, serde_json::json!({}));
        assert_eq!(
            ex.optional,
            ["feed(first: Int! = 10)", "feed(after: String)"]
        );
    }

    #[test]
    fn a_self_referential_input_expands_once() {
        let records = vec![
            rec("Query", "Query", Kind::Object, None, None, &[]),
            rec("Filter", "Filter", Kind::InputObject, None, None, &[]),
            rec(
                "Filter.and",
                "and",
                Kind::InputField,
                Some("Filter"),
                // Filter refers to itself — the expansion must terminate
                Some("[Filter!]"),
                &[],
            ),
            rec(
                "Filter.eq",
                "eq",
                Kind::InputField,
                Some("Filter"),
                Some("String"),
                &[],
            ),
            rec(
                "Query.search",
                "search",
                Kind::Query,
                Some("Query"),
                Some("Int!"),
                &["filter: Filter!"],
            ),
        ];
        let target = records.iter().find(|r| r.path == "Query.search").unwrap();
        let ex = build(target, &records, Some(1)).unwrap();
        // The cycle closes as a placeholder rather than recursing: `and`'s
        // element type is the object it sits inside, whose shape is right there.
        assert_eq!(
            ex.variables,
            serde_json::json!({
                "filter": { "and": ["<Filter!>"], "eq": "<String>" }
            })
        );
    }

    /// The point of the whole module: what it prints must parse as GraphQL.
    #[test]
    fn every_drafted_operation_is_valid_graphql() {
        for path in [
            "Query.user",
            "Query.count",
            "Mutation.save",
            "User.posts",
            "User.name",
        ] {
            let ex = build_for(path);
            graphql_parser::parse_query::<String>(&ex.operation).unwrap_or_else(|e| {
                panic!("{path} drafted invalid GraphQL: {e}\n{}", ex.operation)
            });
        }
    }

    #[test]
    fn colliding_argument_names_are_disambiguated() {
        let records = vec![
            rec("Query", "Query", Kind::Object, None, None, &[]),
            rec("Role", "Role", Kind::Enum, None, None, &[]),
            rec(
                "Role.ADMIN",
                "ADMIN",
                Kind::EnumValue,
                Some("Role"),
                None,
                &[],
            ),
            rec("Thing", "Thing", Kind::Object, None, None, &[]),
            rec(
                "Query.thing",
                "thing",
                Kind::Query,
                Some("Query"),
                Some("Thing"),
                &["id: ID!"],
            ),
            rec(
                "Thing.child",
                "child",
                Kind::Field,
                Some("Thing"),
                Some("String"),
                &["id: ID!", "role: Role!"],
            ),
        ];
        let target = records.iter().find(|r| r.path == "Thing.child").unwrap();
        let ex = build(target, &records, Some(1)).unwrap();
        // the inner `id` collides with the root's, so it's prefixed
        assert!(
            ex.operation.contains("child(id: $childId, role: $role)"),
            "{}",
            ex.operation
        );
        assert_eq!(
            ex.variables,
            serde_json::json!({ "id": "<ID!>", "childId": "<ID!>", "role": "<Role!>" })
        );
    }
}
