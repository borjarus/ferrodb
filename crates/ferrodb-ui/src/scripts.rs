//! Extension scripts in the UI (phase 9): extractors (export formats,
//! "Copy As") and aggregators (the grid's selection summary), plus the
//! actions managing the scripts folder. Everything runs in the backend's
//! `ScriptService`.

use ferrodb_api::{ExtScriptInfo, ExtScriptKind, ScriptValue};
use ferrodb_ui_kit::{
    gpui::{prelude::*, *},
    kit::{
        Sizable as _, WindowExt as _,
        button::{Button, ButtonVariants as _},
        dialog::DialogFooter,
        input::{Input, InputState},
        notification::Notification,
        v_flex,
    },
};

use crate::{
    AppState,
    i18n::{t, t_args},
    output,
    results::format::ValueKind,
    workspace,
};

/// The aggregator chosen in result grids (`None` = the built-in summary).
#[derive(Clone, Default)]
pub struct SelectedAggregator(pub Option<String>);
impl Global for SelectedAggregator {}

pub fn selected_aggregator(cx: &App) -> Option<String> {
    cx.try_global::<SelectedAggregator>()
        .and_then(|s| s.0.clone())
}

pub fn select_aggregator(id: Option<String>, cx: &mut App) {
    cx.set_global(SelectedAggregator(id));
}

/// Scripts of one kind (empty when the backend has none).
pub fn list(kind: ExtScriptKind, cx: &App) -> Vec<ExtScriptInfo> {
    let backend = AppState::backend(cx);
    let s = backend.scripts();
    if s.available() {
        s.list(kind)
    } else {
        Vec::new()
    }
}

/// A grid cell as a script value (`None` = NULL).
pub fn cell_value(kind: ValueKind, text: Option<String>) -> ScriptValue {
    let Some(text) = text else {
        return ScriptValue::Null;
    };
    match kind {
        ValueKind::Integer => text
            .parse()
            .map(ScriptValue::Int)
            .unwrap_or(ScriptValue::Text(text)),
        ValueKind::Float => text
            .parse()
            .map(ScriptValue::Float)
            .unwrap_or(ScriptValue::Text(text)),
        ValueKind::Bool => match text.as_str() {
            "true" => ScriptValue::Bool(true),
            "false" => ScriptValue::Bool(false),
            _ => ScriptValue::Text(text),
        },
        _ => ScriptValue::Text(text),
    }
}

fn unavailable(window: &mut Window, cx: &mut App) -> bool {
    if AppState::backend(cx).scripts().available() {
        return false;
    }
    window.push_notification(Notification::info(t("scripts-unavailable")), cx);
    true
}

/// Opens `<config>/scripts` in the system file manager.
pub fn open_folder(window: &mut Window, cx: &mut App) {
    if unavailable(window, cx) {
        return;
    }
    let Some(dir) = AppState::backend(cx).scripts().directory() else {
        window.push_notification(Notification::info(t("scripts-no-folder")), cx);
        return;
    };
    for kind in [ExtScriptKind::Extractor, ExtScriptKind::Aggregator] {
        let _ = std::fs::create_dir_all(dir.join(kind.dir_name()));
    }
    cx.open_with_system(&dir);
}

/// Asks for a name, then creates a script from the template and opens it.
pub fn new_script(kind: ExtScriptKind, window: &mut Window, cx: &mut App) {
    if unavailable(window, cx) {
        return;
    }
    let input = cx.new(|cx| InputState::new(window, cx).placeholder(t("scripts-name")));
    input.update(cx, |i, cx| i.focus(window, cx));
    let title = t(match kind {
        ExtScriptKind::Extractor => "action-new-extractor",
        ExtScriptKind::Aggregator => "action-new-aggregator",
    });
    window.open_dialog(cx, move |dialog, _, _| {
        let input2 = input.clone();
        dialog
            .title(title.clone())
            .w(px(420.))
            .child(
                v_flex()
                    .gap_2()
                    .child(div().text_sm().child(t("scripts-name-description")))
                    .child(Input::new(&input).small()),
            )
            .footer(
                DialogFooter::new()
                    .child(
                        Button::new("script-cancel")
                            .label(t("button-cancel"))
                            .on_click(|_, w, cx| w.close_dialog(cx)),
                    )
                    .child(
                        Button::new("script-create")
                            .primary()
                            .label(t("scripts-create"))
                            .on_click(move |_, window, cx| {
                                let name = input2.read(cx).value().trim().to_owned();
                                if name.is_empty() {
                                    return;
                                }
                                window.close_dialog(cx);
                                create(kind, name, window, cx);
                            }),
                    ),
            )
    });
}

/// Creates a script named `name` and opens its file in a console.
pub fn create(kind: ExtScriptKind, name: String, window: &mut Window, cx: &mut App) {
    let backend = AppState::backend(cx);
    let task = cx
        .background_executor()
        .spawn(async move { backend.scripts().create(kind, &name).await });
    window
        .spawn(cx, async move |cx| {
            let r = task.await;
            let _ = cx.update(|window, cx| match r {
                Ok(path) => {
                    output::info(
                        t_args(
                            "scripts-created",
                            &[("path", path.display().to_string().into())],
                        ),
                        cx,
                    );
                    if let Err(e) = workspace::open_file_console(None, path.clone(), window, cx) {
                        window.push_notification(Notification::error(e.to_string()), cx);
                        cx.reveal_path(&path);
                    }
                }
                Err(e) => {
                    output::log(output::Level::Error, e.to_string(), cx);
                    window.push_notification(Notification::error(e.to_string()), cx);
                }
            });
        })
        .detach();
}

/// Re-reads the scripts directory.
pub fn reload(window: &mut Window, cx: &mut App) {
    if unavailable(window, cx) {
        return;
    }
    let backend = AppState::backend(cx);
    let task = cx
        .background_executor()
        .spawn(async move { backend.scripts().reload().await });
    window
        .spawn(cx, async move |cx| {
            let r = task.await;
            let _ = cx.update(|window, cx| {
                match r {
                    Ok(()) => {
                        let backend = AppState::backend(cx);
                        let n = backend.scripts().list(ExtScriptKind::Extractor).len()
                            + backend.scripts().list(ExtScriptKind::Aggregator).len();
                        let msg = t_args("scripts-reloaded", &[("count", n.into())]);
                        output::info(msg.clone(), cx);
                        window.push_notification(Notification::success(msg), cx);
                    }
                    Err(e) => {
                        output::log(output::Level::Error, e.to_string(), cx);
                        window.push_notification(Notification::error(e.to_string()), cx);
                    }
                }
                cx.refresh_windows();
            });
        })
        .detach();
}

/// Button style shared by script pickers.
pub fn picker_button(id: impl Into<ElementId>, label: String) -> Button {
    Button::new(id).ghost().xsmall().label(label)
}
