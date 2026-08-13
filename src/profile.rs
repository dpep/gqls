//! Phase timing for `--profile`.
//!
//! Off by default and built to cost nothing when off: a span is created around
//! each phase, but with profiling disabled it holds no clock reading, takes no
//! lock, allocates nothing, and its `Drop` returns immediately. The only work
//! left on the hot path is one relaxed atomic load per phase.
//!
//! Phases are a flat list — names carry their own structure
//! (`semantic: model load`) rather than a nesting scheme, because the report is
//! read top to bottom and the total is wall time, not a sum.
//!
//! A span does track how deeply it nests, for one reason: the report ends with
//! the time *no* phase claimed. Un-instrumented work is exactly the work a
//! profile is supposed to find, and a report that silently omits it points the
//! reader at the phases that are fast rather than at the seconds that aren't.
//! Only outermost spans count toward that arithmetic, so a nested phase can't
//! be subtracted twice.

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

static ENABLED: AtomicBool = AtomicBool::new(false);
static PHASES: Mutex<Vec<Phase>> = Mutex::new(Vec::new());

thread_local! {
    /// How many spans are open on this thread — 0 for an outermost one.
    static DEPTH: Cell<usize> = const { Cell::new(0) };
}

/// One measured phase.
pub struct Phase {
    pub name: &'static str,
    pub elapsed: Duration,
    /// What the phase did — record counts, bytes, a cache verdict.
    pub note: Option<String>,
    /// Spans open around this one when it started; 0 means its time is its
    /// own, not a slice of another phase's.
    pub depth: usize,
}

/// Turn profiling on. Called once from the CLI when `--profile` is passed.
pub fn enable() {
    ENABLED.store(true, Ordering::Relaxed);
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Start timing a phase. The returned span records it when dropped; with
/// profiling off it is inert.
pub fn span(name: &'static str) -> Span {
    // No clock read, and no thread-local touched, when disabled — this is the
    // whole point.
    if !enabled() {
        return Span {
            name,
            start: None,
            note: None,
            depth: 0,
        };
    }
    let depth = DEPTH.with(|d| {
        let was = d.get();
        d.set(was + 1);
        was
    });
    Span {
        name,
        start: Some(Instant::now()),
        note: None,
        depth,
    }
}

pub struct Span {
    name: &'static str,
    start: Option<Instant>,
    note: Option<String>,
    depth: usize,
}

impl Span {
    /// Attach detail to this phase. The closure runs only when profiling is on,
    /// so formatting a note never costs anything in a normal run.
    pub fn note(&mut self, f: impl FnOnce() -> String) {
        if self.start.is_some() {
            self.note = Some(f());
        }
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        let Some(start) = self.start else { return };
        DEPTH.with(|d| d.set(self.depth));
        if let Ok(mut phases) = PHASES.lock() {
            phases.push(Phase {
                name: self.name,
                elapsed: start.elapsed(),
                note: self.note.take(),
                depth: self.depth,
            });
        }
    }
}

/// Wall time no outermost phase accounted for — un-instrumented work, and the
/// first thing to look at when it dominates.
fn unaccounted(phases: &[Phase], total: Duration) -> Duration {
    let measured: Duration = phases
        .iter()
        .filter(|p| p.depth == 0)
        .map(|p| p.elapsed)
        .sum();
    total.saturating_sub(measured)
}

/// Every phase recorded so far, in the order they finished.
pub fn phases() -> Vec<Phase> {
    PHASES
        .lock()
        .map(|mut p| std::mem::take(&mut *p))
        .unwrap_or_default()
}

/// The report, as lines ready for stderr. Empty when nothing was measured.
pub fn report(total: Duration) -> Vec<String> {
    let phases = phases();
    if phases.is_empty() {
        return Vec::new();
    }
    let width = phases
        .iter()
        .map(|p| p.name.len())
        .max()
        .unwrap_or(0)
        .max("unaccounted".len());
    let mut out: Vec<String> = phases
        .iter()
        .map(|p| {
            let note = p.note.as_deref().unwrap_or_default();
            format!(
                "  {:<width$}  {:>8}  {note}",
                p.name,
                ms(p.elapsed),
                width = width
            )
            .trim_end()
            .to_string()
        })
        .collect();
    // Only when there's something to explain: a sub-millisecond remainder is
    // rounding, not a phase someone forgot to instrument.
    let gap = unaccounted(&phases, total);
    if gap >= Duration::from_millis(1) {
        out.push(format!(
            "  {:<width$}  {:>8}  not instrumented",
            "unaccounted",
            ms(gap),
            width = width
        ));
    }
    // No trailing empty cell: padding one to eight columns left ten spaces at
    // the end of the line.
    out.push(format!("  {}", "─".repeat(width.min(20))));
    out.push(format!(
        "  {:<width$}  {:>8}",
        "total",
        ms(total),
        width = width
    ));
    out
}

/// Phases as JSON, for storing a baseline and diffing runs.
pub fn json(total: Duration) -> serde_json::Value {
    let phases = phases();
    serde_json::json!({
        "total_ms": total.as_secs_f64() * 1000.0,
        "unaccounted_ms": unaccounted(&phases, total).as_secs_f64() * 1000.0,
        "phases": phases
            .iter()
            .map(|p| serde_json::json!({
                "name": p.name,
                "ms": p.elapsed.as_secs_f64() * 1000.0,
                "note": p.note,
                "depth": p.depth,
            }))
            .collect::<Vec<_>>(),
    })
}

fn ms(d: Duration) -> String {
    format!("{:.1}ms", d.as_secs_f64() * 1000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ENABLED` and `PHASES` are process-wide, so these two tests can't run at
    /// the same time — without this the enabled one flips the flag under the
    /// other, which then fails on a machine-speed coincidence.
    static SERIAL: Mutex<()> = Mutex::new(());

    fn serialize() -> std::sync::MutexGuard<'static, ()> {
        SERIAL.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn a_span_is_inert_when_profiling_is_off() {
        let _guard = serialize();
        // the default; nothing is recorded and no clock is read
        let mut s = span("off");
        s.note(|| panic!("the note closure must not run when disabled"));
        drop(s);
        assert!(phases().is_empty());
    }

    #[test]
    fn only_outermost_phases_count_against_the_total() {
        // The arithmetic that makes the report honest: a nested phase is a
        // slice of its parent, so counting both would hide un-instrumented
        // time behind a negative remainder.
        let phases = vec![
            Phase {
                name: "cache: read",
                elapsed: Duration::from_millis(5),
                note: None,
                depth: 1,
            },
            Phase {
                name: "load",
                elapsed: Duration::from_millis(10),
                note: None,
                depth: 0,
            },
        ];
        assert_eq!(
            unaccounted(&phases, Duration::from_millis(30)),
            Duration::from_millis(20)
        );
        // and time can never be over-explained into a negative
        assert_eq!(
            unaccounted(&phases, Duration::from_millis(1)),
            Duration::ZERO
        );
    }

    #[test]
    fn a_nested_span_knows_it_is_nested() {
        let _guard = serialize();
        enable();
        {
            let _outer = span("outer");
            let _inner = span("inner");
        }
        let recorded = phases();
        let depths: Vec<usize> = recorded.iter().map(|p| p.depth).collect();
        // inner finishes first, so it's recorded first
        assert_eq!(depths, [1, 0]);
        ENABLED.store(false, Ordering::Relaxed);
    }

    #[test]
    fn an_enabled_span_records_its_name_and_note() {
        let _guard = serialize();
        enable();
        {
            let mut s = span("on");
            s.note(|| "42 records".to_string());
        }
        let recorded = phases();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].name, "on");
        assert_eq!(recorded[0].note.as_deref(), Some("42 records"));
        // phases() drains, so a second call sees nothing
        assert!(phases().is_empty());
        ENABLED.store(false, Ordering::Relaxed);
    }
}
