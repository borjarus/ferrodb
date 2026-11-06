//! What the Inspector panel shows: the value panel of the selected cell,
//! field or key, or the properties of the selected explorer object.
//! Views publish with [`show`]; the panel observes the shared entity.

use ferrodb_ui_kit::gpui::{App, AppContext as _, Entity, Global};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Inspection {
    /// e.g. `customers.name` or `app:user:12`.
    pub title: String,
    /// Secondary line, e.g. the type.
    pub subtitle: String,
    /// Full value (shown in a read-only editor).
    pub value: Option<String>,
    /// Property list (record view, object properties).
    pub fields: Vec<(String, String)>,
}

struct InspectorGlobal(Entity<Inspection>);
impl Global for InspectorGlobal {}

pub fn init(cx: &mut App) {
    let e = cx.new(|_| Inspection::default());
    cx.set_global(InspectorGlobal(e));
}

pub fn entity(cx: &App) -> Option<Entity<Inspection>> {
    cx.try_global::<InspectorGlobal>().map(|g| g.0.clone())
}

pub fn show(inspection: Inspection, cx: &mut App) {
    if let Some(e) = entity(cx) {
        e.update(cx, |i, cx| {
            if *i != inspection {
                *i = inspection;
                cx.notify();
            }
        });
    }
}

pub fn current(cx: &App) -> Inspection {
    entity(cx).map(|e| e.read(cx).clone()).unwrap_or_default()
}
