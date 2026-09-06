// SPDX-License-Identifier: GPL-2.0-only

//! Diagnostics sink.
//!
//! Section 7 of `docs/MIGRATION-PLAN.md` requires that nothing in this crate
//! write to a global stream: the CUPS filter routes these lines to stderr with
//! the prefixes CUPS recognises, while the printer application routes them to
//! `papplLogJob`, where there is no stderr to route them to. The crate states
//! only the level and the text.

/// Severity, named after the prefixes the CUPS filter interface defines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Debug,
    Info,
    Warning,
    Error,
    /// `PAGE: <number> <copies>` — page accounting, not severity.
    Page,
}

/// Where a front end wants the engine's diagnostics to go.
pub trait Log {
    fn log(&self, level: Level, message: &str);
}

/// Discards everything. Used by tests that assert on returned errors rather
/// than on diagnostics.
pub struct NoLog;

impl Log for NoLog {
    fn log(&self, _level: Level, _message: &str) {}
}
