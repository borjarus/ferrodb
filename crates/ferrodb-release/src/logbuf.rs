//! The last log lines of the process, kept in memory for crash reports.
//!
//! [`log_layer`] is a `tracing` layer that formats every event it sees into
//! a bounded ring buffer ([`LOG_LINES`] lines, secrets masked on the way
//! in). The panic hook copies the buffer into the report. Nothing is written
//! anywhere by this layer.

use std::{
    collections::VecDeque,
    fmt::Write as _,
    sync::{Mutex, OnceLock},
};

use tracing::{
    Event, Subscriber,
    field::{Field, Visit},
};
use tracing_subscriber::{Layer, layer::Context};

use crate::mask::mask_secrets;

/// Lines kept for a crash report.
pub const LOG_LINES: usize = 200;
/// Longest line kept (characters).
const MAX_LINE: usize = 2000;

fn ring() -> &'static Mutex<VecDeque<String>> {
    static RING: OnceLock<Mutex<VecDeque<String>>> = OnceLock::new();
    RING.get_or_init(|| Mutex::new(VecDeque::with_capacity(LOG_LINES)))
}

/// Appends a line (masked) to the buffer.
pub fn push_line(line: &str) {
    let mut line = mask_secrets(line);
    if line.len() > MAX_LINE {
        let mut cut = MAX_LINE;
        while !line.is_char_boundary(cut) {
            cut -= 1;
        }
        line.truncate(cut);
        line.push('…');
    }
    let mut r = ring().lock().unwrap_or_else(|p| p.into_inner());
    if r.len() == LOG_LINES {
        r.pop_front();
    }
    r.push_back(line);
}

/// The buffered lines, oldest first. Never blocks: a panic while the buffer
/// is locked (inside the logger) gives an empty tail instead of a deadlock.
pub fn tail() -> Vec<String> {
    match ring().try_lock() {
        Ok(r) => r.iter().cloned().collect(),
        Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner().iter().cloned().collect(),
        Err(std::sync::TryLockError::WouldBlock) => Vec::new(),
    }
}

/// Empties the buffer (tests).
pub fn clear() {
    ring().lock().unwrap_or_else(|p| p.into_inner()).clear();
}

/// The `tracing` layer feeding the buffer.
pub fn log_layer<S: Subscriber>() -> impl Layer<S> + Send + Sync + 'static {
    RingLayer
}

struct RingLayer;

struct Fields {
    message: String,
    rest: String,
}

impl Visit for Fields {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message.push_str(value);
        } else {
            let _ = write!(self.rest, " {}={value}", field.name());
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.message, "{value:?}");
        } else {
            let _ = write!(self.rest, " {}={value:?}", field.name());
        }
    }
}

impl<S: Subscriber> Layer<S> for RingLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        let mut f = Fields {
            message: String::new(),
            rest: String::new(),
        };
        event.record(&mut f);
        let time = chrono::Utc::now().format("%H:%M:%S%.3f");
        push_line(&format!(
            "{time} {:5} {}: {}{}",
            meta.level(),
            meta.target(),
            f.message,
            f.rest
        ));
    }
}

#[cfg(test)]
mod tests {
    use tracing_subscriber::layer::SubscriberExt as _;

    use super::*;

    #[test]
    fn events_are_buffered_masked() {
        let subscriber = tracing_subscriber::registry().with(log_layer());
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(url = "postgres://u:hunter2@db/x", "connecting marker-17");
        });
        let t = tail();
        let line = t
            .iter()
            .find(|l| l.contains("marker-17"))
            .expect("line buffered");
        assert!(line.contains("INFO"), "{line}");
        assert!(line.contains("postgres://u:***@db/x"), "{line}");
        assert!(!line.contains("hunter2"));
    }

    #[test]
    fn buffer_is_bounded() {
        for i in 0..LOG_LINES + 10 {
            push_line(&format!("bounded {i}"));
        }
        let t = tail();
        assert!(t.len() <= LOG_LINES);
        assert!(t.iter().any(|l| l == &format!("bounded {}", LOG_LINES + 9)));
    }
}
