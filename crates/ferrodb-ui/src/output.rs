//! Output log shown in the "Output" panel. Views append human-readable lines;
//! the panel observes the log entity.

use std::time::SystemTime;

use ferrodb_ui_kit::gpui::{App, AppContext, Entity, Global};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Info,
    Warning,
    Error,
}

#[derive(Clone, Debug)]
pub struct Line {
    pub at: SystemTime,
    pub level: Level,
    pub text: String,
}

/// Bounded in-memory log.
#[derive(Default)]
pub struct OutputLog {
    pub lines: Vec<Line>,
}

const MAX_LINES: usize = 5_000;

struct OutputGlobal(Entity<OutputLog>);
impl Global for OutputGlobal {}

pub fn init(cx: &mut App) {
    let log = cx.new(|_| OutputLog::default());
    cx.set_global(OutputGlobal(log));
}

pub fn entity(cx: &App) -> Entity<OutputLog> {
    cx.global::<OutputGlobal>().0.clone()
}

pub fn log(level: Level, text: impl Into<String>, cx: &mut App) {
    let text = text.into();
    match level {
        Level::Info => tracing::info!(target: "output", "{text}"),
        Level::Warning => tracing::warn!(target: "output", "{text}"),
        Level::Error => tracing::error!(target: "output", "{text}"),
    }
    entity(cx).update(cx, |log, cx| {
        log.lines.push(Line {
            at: SystemTime::now(),
            level,
            text,
        });
        if log.lines.len() > MAX_LINES {
            let excess = log.lines.len() - MAX_LINES;
            log.lines.drain(..excess);
        }
        cx.notify();
    });
}

pub fn info(text: impl Into<String>, cx: &mut App) {
    log(Level::Info, text, cx);
}
