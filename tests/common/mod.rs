//! Shared helpers for tests that drive the real binary.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Fail loudly if the binary under test predates the sources, because cargo
/// sometimes insists it's fresh when it isn't.
///
/// The mechanism, reproduced: cargo decides a unit is fresh by comparing each
/// source's mtime against a reference file under
/// `target/debug/.fingerprint/<pkg>/dep-*`. If that reference ever acquires a
/// *future* mtime, the unit is fresh forever — `touch` on a source can't beat
/// it, `cargo build` reports `Finished` without compiling, and
/// `CARGO_LOG=cargo::core::compiler::fingerprint=info` prints nothing at all,
/// because from cargo's point of view there is nothing to say. Only
/// `cargo clean -p` heals it. `script/check.sh` opens with exactly that clean,
/// so the gate is safe; an ad-hoc `cargo test` is not.
///
/// What planted the future timestamp is still unknown — twice now the fix
/// destroyed the evidence. Hence the last line of the panic message.
pub fn assert_binary_is_current(bin: &str) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let bin_time = mtime(Path::new(bin)).expect("the binary under test should exist");

    // Exactly the inputs that dirty the binary's fingerprint. Tests and
    // examples don't feed it, so a newer test file is not a signal.
    let mut newest = mtime(&root.join("Cargo.toml")).unwrap_or(SystemTime::UNIX_EPOCH);
    let mut newest_path = root.join("Cargo.toml");
    walk(&root.join("src"), &mut |p| {
        if p.extension().is_some_and(|x| x == "rs") {
            if let Some(t) = mtime(p) {
                if t > newest {
                    newest = t;
                    newest_path = p.to_path_buf();
                }
            }
        }
    });

    // Equality passes: a source and a binary written in the same instant are
    // consistent, and APFS timestamps are precise enough that this isn't slack.
    assert!(
        bin_time >= newest,
        "the test binary is older than the sources it should have been built from.\n\
         \x20 binary: {bin}\n\
         \x20 newer:  {}\n\n\
         cargo believes this build is fresh when it is not — a fingerprint file \
         under target/debug/.fingerprint/ has picked up a future mtime, which \
         makes the unit fresh permanently and silently.\n\n\
         Fix:     cargo clean -p gqls-cli && cargo test\n\
         Or run:  script/check.sh   (it cleans first, so it never hits this)\n\n\
         Before cleaning, please capture the evidence — the cause is still \
         unidentified and the fix destroys it:\n\
         \x20 ls -laT target/debug/.fingerprint/gqls-cli-*/",
        newest_path.display()
    );
}

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).ok()?.modified().ok()
}

fn walk(dir: &Path, f: &mut impl FnMut(&Path)) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, f);
        } else {
            f(&p);
        }
    }
}
