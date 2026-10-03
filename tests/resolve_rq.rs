//! `-R` against rq's real output contract.
//!
//! gqls shells out to `rq`, so its half of the contract is the arguments it
//! passes and the rows it reads back. Both drifted once already: rq removed a
//! flag gqls passed, and every `-R` failed with a usage error. These tests put
//! a stub `rq` on PATH that accepts only the flags real rq accepts and replays
//! the shapes rq 0.60.x emits — a hit, a hit folded from several declarations,
//! a hit from a checkout still being indexed, a provisional answer (exit 2), a
//! miss, and an error object — and one test drives the real binary when it's
//! installed, so a future contract change is caught rather than shipped.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("gqls-rq-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch dir");
    dir
}

const SCHEMA: &str = "type Query {\n  widget: Widget\n}\ntype Widget {\n  id: ID\n}\n";

/// A stub `rq` in `dir/bin` that rejects any flag real rq doesn't take (the
/// way real rq does: a JSON error object on stdout, exit 64), logs its
/// arguments, then prints `stdout` and exits `code`.
fn stub_rq(dir: &Path, stdout: &str, code: i32) -> PathBuf {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).expect("a bin dir");
    std::fs::write(dir.join("rows.ndjson"), stdout).expect("writing rows");
    let script = format!(
        r#"#!/bin/sh
printf '%s\n' "$@" > '{log}'
while [ $# -gt 0 ]; do
  case "$1" in
    --ndjson|-J|--verbose|-v) ;;
    --limit|-l) shift ;;
    *) echo "{{\"code\":64,\"error\":\"error: unexpected argument '$1' found\",\"kind\":\"usage\"}}"
       echo "error: unexpected argument '$1' found" >&2
       exit 64 ;;
  esac
  shift
done
cat > /dev/null
cat '{rows}'
exit {code}
"#,
        log = dir.join("args.log").display(),
        rows = dir.join("rows.ndjson").display(),
    );
    let path = bin.join("rq");
    std::fs::write(&path, script).expect("writing the stub");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    bin
}

/// `gqls Query.widget -R` with `bin` first on PATH, in its own cache.
fn resolve(dir: &Path, bin: &Path, extra: &[&str]) -> Output {
    common::assert_binary_is_current(env!("CARGO_BIN_EXE_gqls"));
    let schema = dir.join("schema.graphql");
    std::fs::write(&schema, SCHEMA).expect("writing the schema");
    let path = format!("{}:/usr/bin:/bin", bin.display());
    Command::new(env!("CARGO_BIN_EXE_gqls"))
        .arg("Query.widget")
        .arg(&schema)
        .args(["-R", "--code"])
        .arg(dir)
        .args(extra)
        .env("PATH", path)
        .env("XDG_CACHE_HOME", dir.join("cache"))
        .output()
        .expect("gqls should be runnable")
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn json(out: &Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}\n{}", text(&out.stdout), text(&out.stderr)))
}

const HIT: &str = r#"{"query":"Resolvers::Widget","name":"Widget","kind":"class","language":"ruby","file":"app/graphql/resolvers/widget.rb","line":2,"end_line":4,"parent":"Resolvers","repo":"local:/r","source":"index","confidence":1.0,"features":["exact"],"signature":"class Widget","declarations":2,"also_in":["lib/widget_ext.rb:2"],"total":1}"#;
const MISS: &str = r#"{"query":"Queries::Widget","status":"no_match"}"#;
const WARMING: &str =
    r#"{"read":0,"of":3,"interrupted":false,"hint":"rq is still indexing this checkout"}"#;

#[test]
fn gqls_passes_rq_only_flags_rq_takes() {
    // rq 0.52 removed `--no-record`; passing it turned every -R into a usage
    // error. The stub rejects what real rq rejects.
    let dir = scratch("args");
    let bin = stub_rq(&dir, &format!("{HIT}\n{MISS}\n"), 0);
    let out = resolve(&dir, &bin, &[]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let args = std::fs::read_to_string(dir.join("args.log")).expect("the stub ran");
    assert_eq!(
        args.lines().collect::<Vec<_>>(),
        ["--ndjson", "--limit", "10"]
    );
}

#[test]
fn a_hit_declared_in_several_places_says_where_else() {
    let dir = scratch("also");
    let bin = stub_rq(&dir, &format!("{HIT}\n{MISS}\n"), 0);

    let out = resolve(&dir, &bin, &[]);
    let stdout = text(&out.stdout);
    assert!(
        stdout.contains("app/graphql/resolvers/widget.rb:2"),
        "{stdout}"
    );
    assert!(stdout.contains("also in lib/widget_ext.rb:2"), "{stdout}");

    let out = resolve(&dir, &bin, &["-j"]);
    let hit = &json(&out)[0];
    assert_eq!(hit["also_in"], serde_json::json!(["lib/widget_ext.rb:2"]));
    assert_eq!(hit["declarations"], 2);
    assert_eq!(hit["provisional"], false);
    // rq's number, named as rq's: gqls's own verdict is `loose`
    assert_eq!(hit["rq_confidence"], 1.0);
    assert!(hit.get("confidence").is_none(), "{hit}");
}

#[test]
fn a_hit_from_a_checkout_still_being_indexed_says_so() {
    let dir = scratch("warmhit");
    let hit = HIT.replace(
        r#""confidence":1.0"#,
        &format!(r#""confidence":0.0,"warming":{WARMING}"#),
    );
    let bin = stub_rq(&dir, &format!("{hit}\n{MISS}\n"), 0);

    let out = resolve(&dir, &bin, &[]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let err = text(&out.stderr);
    assert!(err.contains("rq still indexing: 0 of 3 files"), "{err}");

    let out = resolve(&dir, &bin, &["-j"]);
    let hit = &json(&out)[0];
    assert_eq!(hit["warming"]["of"], 3, "{hit}");
    assert_eq!(hit["warming"]["hint"], "rq is still indexing this checkout");
}

#[test]
fn a_provisional_answer_is_an_answer_marked_provisional() {
    // rq exits 2 when a file it hasn't read could still beat what it found,
    // and hands back what it has as `provisional`. Not a failure.
    let dir = scratch("provisional");
    let inner = HIT.replace(r#""query":"Resolvers::Widget","#, "");
    let row = format!(
        r#"{{"provisional":[{inner}],"query":"Resolvers::Widget","status":"warming","warming":{WARMING}}}"#
    );
    let bin = stub_rq(&dir, &format!("{row}\n{MISS}\n"), 2);

    let out = resolve(&dir, &bin, &[]);
    let (stdout, err) = (text(&out.stdout), text(&out.stderr));
    assert_eq!(out.status.code(), Some(0), "{err}");
    assert!(
        stdout.contains("app/graphql/resolvers/widget.rb:2") && stdout.contains("(provisional)"),
        "{stdout}"
    );
    assert!(err.contains("rq still indexing: 0 of 3 files"), "{err}");
    assert!(!err.contains("rq failed"), "{err}");

    let out = resolve(&dir, &bin, &["-j"]);
    let hit = &json(&out)[0];
    assert_eq!(hit["provisional"], true, "{hit}");
    assert_eq!(hit["warming"]["read"], 0, "{hit}");
}

#[test]
fn a_miss_while_rq_is_still_indexing_is_not_an_answer() {
    // "nothing yet" isn't "nothing": exit 1 with rq's hint, not exit 0 under a
    // "no code definition found" that reads as definitive.
    let dir = scratch("warmmiss");
    let row = format!(r#"{{"query":"Resolvers::Widget","status":"warming","warming":{WARMING}}}"#);
    let bin = stub_rq(&dir, &format!("{row}\n{MISS}\n"), 2);

    let out = resolve(&dir, &bin, &[]);
    let err = text(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{err}");
    assert!(err.contains("0 of 3 files"), "{err}");
    assert!(err.contains("rq is still indexing this checkout"), "{err}");
    assert!(!err.contains("no code definition found"), "{err}");
}

#[test]
fn a_plain_miss_is_still_an_answer() {
    let dir = scratch("miss");
    let rows = format!(
        "{}\n{MISS}\n",
        r#"{"query":"Resolvers::Widget","status":"no_match"}"#
    );
    let bin = stub_rq(&dir, &rows, 1);
    let out = resolve(&dir, &bin, &[]);
    let err = text(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{err}");
    assert!(err.contains("no code definition found"), "{err}");
}

#[test]
fn an_rq_error_says_what_rq_said() {
    let dir = scratch("error");
    let bin = stub_rq(
        &dir,
        r#"{"code":74,"error":"rq: can't open the index at /x/rq.db","kind":"database"}"#,
        74,
    );
    let out = resolve(&dir, &bin, &[]);
    let err = text(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{err}");
    assert!(err.contains("can't open the index at /x/rq.db"), "{err}");
    assert!(err.contains("exit 74"), "{err}");
    assert!(!err.contains("Some("), "{err}");
}

/// The real `rq` on PATH, if there is one.
fn installed_rq() -> Option<PathBuf> {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .map(|d| d.join("rq"))
        .find(|p| p.is_file())
}

#[test]
fn the_installed_rq_answers_a_resolve() {
    // The stub above is only as good as its author's reading of rq's contract.
    // This drives the real one, isolated from the user's index.
    let Some(rq) = installed_rq() else {
        eprintln!("skipping: no rq on PATH");
        return;
    };
    let dir = scratch("real");
    let repo = dir.join("repo");
    for (file, body) in [
        (
            "app/graphql/resolvers/widget.rb",
            "module Resolvers\n  class Widget\n    def resolve; end\n  end\nend\n",
        ),
        (
            "lib/widget_ext.rb",
            "module Resolvers\n  class Widget\n    def extra; end\n  end\nend\n",
        ),
    ] {
        let p = repo.join(file);
        std::fs::create_dir_all(p.parent().expect("a parent")).expect("mkdir");
        std::fs::write(p, body).expect("writing ruby");
    }
    let git = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .current_dir(&repo)
            .output()
            .is_ok_and(|o| o.status.success())
    };
    if !(git(&["init", "-q"])
        && git(&["add", "-A"])
        && git(&[
            "-c",
            "user.email=t@example.com",
            "-c",
            "user.name=t",
            "commit",
            "-qm",
            "init",
        ]))
    {
        eprintln!("skipping: git unavailable");
        return;
    }
    let schema = dir.join("schema.graphql");
    std::fs::write(&schema, SCHEMA).expect("writing the schema");
    let path = format!(
        "{}:{}",
        rq.parent().expect("a bin dir").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = Command::new(env!("CARGO_BIN_EXE_gqls"))
        .arg("Query.widget")
        .arg(&schema)
        .args(["-R", "-j", "--code"])
        .arg(&repo)
        .env("PATH", path)
        .env("XDG_CACHE_HOME", dir.join("cache"))
        .env("RQ_DB", dir.join("rq.db"))
        .env("RQ_WARM_DETACH", "0")
        .output()
        .expect("gqls should be runnable");
    assert!(out.status.success(), "{}", text(&out.stderr));
    let hits = json(&out);
    let top = &hits[0];
    assert_eq!(top["file"], "app/graphql/resolvers/widget.rb", "{hits}");
    assert_eq!(top["via"], "Resolvers::Widget", "{hits}");
    assert_eq!(top["also_in"], serde_json::json!(["lib/widget_ext.rb:2"]));
    let _ = std::fs::remove_dir_all(&dir);
}
