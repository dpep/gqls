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

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

static ENABLED: AtomicBool = AtomicBool::new(false);
static PHASES: Mutex<Vec<Phase>> = Mutex::new(Vec::new());

/// One measured phase.
pub struct Phase {
    pub name: &'static str,
    pub elapsed: Duration,
    /// What the phase did — record counts, bytes, a cache verdict.
    pub note: Option<String>,
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
    Span {
        name,
        // No clock read at all when disabled — this is the whole point.
        start: enabled().then(Instant::now),
        note: None,
    }
}

pub struct Span {
    name: &'static str,
    start: Option<Instant>,
    note: Option<String>,
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
        if let Ok(mut phases) = PHASES.lock() {
            phases.push(Phase {
                name: self.name,
                elapsed: start.elapsed(),
                note: self.note.take(),
            });
        }
    }
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
        .max(5);
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
    out.push(format!(
        "  {:<width$}  {:>8}",
        "─".repeat(width.min(20)),
        "",
        width = width
    ));
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
        "phases": phases
            .iter()
            .map(|p| serde_json::json!({
                "name": p.name,
                "ms": p.elapsed.as_secs_f64() * 1000.0,
                "note": p.note,
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
