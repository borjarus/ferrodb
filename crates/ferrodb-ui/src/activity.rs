//! UI-side activity shared by panels: queries started by consoles (Services
//! panel, cancellation) and Local History snapshots of console texts.
//! (Persistent Local History moves to `ferrodb-storage` with the core.)

use std::time::{Instant, SystemTime};

use ferrodb_api::{CancelHandle, DataSourceId, QueryId};
use ferrodb_ui_kit::gpui::{App, AppContext as _, Entity, Global};

#[derive(Clone)]
pub struct RunningQuery {
    pub id: QueryId,
    pub data_source: DataSourceId,
    pub text: String,
    pub started: Instant,
    pub cancel: CancelHandle,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub data_source: Option<DataSourceId>,
    pub text: String,
    pub at: SystemTime,
    /// Why it was taken (`executed`, `restored`…), a Fluent id.
    pub reason: &'static str,
}

/// Snapshots kept in memory.
const MAX_SNAPSHOTS: usize = 200;

#[derive(Default)]
pub struct Activity {
    pub running: Vec<RunningQuery>,
    pub snapshots: Vec<Snapshot>,
}

struct ActivityGlobal(Entity<Activity>);
impl Global for ActivityGlobal {}

pub fn init(cx: &mut App) {
    let e = cx.new(|_| Activity::default());
    cx.set_global(ActivityGlobal(e));
}

pub fn entity(cx: &App) -> Option<Entity<Activity>> {
    cx.try_global::<ActivityGlobal>().map(|g| g.0.clone())
}

fn update(cx: &mut App, f: impl FnOnce(&mut Activity)) {
    if let Some(e) = entity(cx) {
        e.update(cx, |a, cx| {
            f(a);
            cx.notify();
        });
    }
}

pub fn started(q: RunningQuery, cx: &mut App) {
    update(cx, |a| a.running.push(q));
}

pub fn finished(id: QueryId, cx: &mut App) {
    update(cx, |a| a.running.retain(|q| q.id != id));
}

pub fn cancel(id: QueryId, cx: &mut App) {
    if let Some(e) = entity(cx)
        && let Some(q) = e.read(cx).running.iter().find(|q| q.id == id)
    {
        q.cancel.cancel();
    }
}

/// Records a snapshot unless it repeats the newest one for the same source.
pub fn snapshot(
    data_source: Option<DataSourceId>,
    text: String,
    reason: &'static str,
    cx: &mut App,
) {
    if text.trim().is_empty() {
        return;
    }
    update(cx, |a| {
        if a.snapshots
            .last()
            .is_some_and(|s| s.text == text && s.data_source == data_source)
        {
            return;
        }
        a.snapshots.push(Snapshot {
            data_source,
            text,
            at: SystemTime::now(),
            reason,
        });
        if a.snapshots.len() > MAX_SNAPSHOTS {
            a.snapshots.remove(0);
        }
    });
}
