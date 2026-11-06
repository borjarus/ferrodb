//! Operation preview → execution (plan 5.6): every edit of a result goes
//! through a dialog listing the statements / commands the backend will run.

use std::rc::Rc;

use ferrodb_api::ChangeSet;
use ferrodb_ui_kit::{
    gpui::{prelude::*, *},
    kit::{
        ActiveTheme, WindowExt as _,
        button::{Button, ButtonVariants as _},
        dialog::DialogFooter,
        notification::Notification,
        v_flex,
    },
};

use super::ResultModel;
use crate::{
    AppState,
    i18n::{t, t_args},
    output,
};

/// Callback run after a successful apply.
type OnApplied = Rc<dyn Fn(&mut Window, &mut App)>;

/// Last preview shown (inspected by UI tests).
#[derive(Clone, Default)]
pub struct LastPreview(pub Vec<String>);
impl Global for LastPreview {}

/// Fetches the preview, shows it, and applies the changes on confirmation.
/// `on_applied` runs after a successful apply (e.g. to clear pending edits).
pub fn review_and_apply(
    model: &Entity<ResultModel>,
    changes: ChangeSet,
    on_applied: impl Fn(&mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) {
    if changes.changes.is_empty() {
        return;
    }
    let result = model.read(cx).id;
    let backend = AppState::backend(cx);
    let preview = {
        let backend = backend.clone();
        let changes = changes.clone();
        cx.background_executor()
            .spawn(async move { backend.queries().preview_changes(result, changes).await })
    };
    let on_applied: OnApplied = Rc::new(on_applied);
    window
        .spawn(cx, async move |cx| {
            let statements = match preview.await {
                Ok(s) => s,
                Err(e) => {
                    let _ = cx.update(|window, cx| {
                        window.push_notification(Notification::error(e.to_string()), cx)
                    });
                    return;
                }
            };
            let _ = cx.update(|window, cx| {
                cx.set_global(LastPreview(statements.clone()));
                open_dialog(statements, result, changes, backend, on_applied, window, cx);
            });
        })
        .detach();
}

fn open_dialog(
    statements: Vec<String>,
    result: ferrodb_api::ResultId,
    changes: ChangeSet,
    backend: std::sync::Arc<dyn ferrodb_api::Backend>,
    on_applied: OnApplied,
    window: &mut Window,
    cx: &mut App,
) {
    let text: SharedString = statements.join("\n").into();
    let count = changes.changes.len();
    window.open_dialog(cx, move |dialog, _, cx| {
        let apply = {
            let backend = backend.clone();
            let changes = changes.clone();
            let on_applied = on_applied.clone();
            move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                window.close_dialog(cx);
                let backend = backend.clone();
                let changes = changes.clone();
                let on_applied = on_applied.clone();
                let task = cx
                    .background_executor()
                    .spawn(async move { backend.queries().apply_changes(result, changes).await });
                window
                    .spawn(cx, async move |cx| {
                        let report = task.await;
                        let _ = cx.update(|window, cx| match report {
                            Ok(r) => {
                                for s in &r.statements {
                                    output::info(s.clone(), cx);
                                }
                                let msg = t_args("changes-applied", &[("count", r.applied.into())]);
                                window.push_notification(Notification::success(msg), cx);
                                on_applied(window, cx);
                            }
                            Err(e) => {
                                output::log(output::Level::Error, e.to_string(), cx);
                                window.push_notification(Notification::error(e.to_string()), cx);
                            }
                        });
                    })
                    .detach();
            }
        };
        dialog
            .title(t_args("changes-preview-title", &[("count", count.into())]))
            .w(px(640.))
            .child(
                v_flex()
                    .gap_2()
                    .child(t("changes-preview-description"))
                    .child(
                        div()
                            .id("changes-preview")
                            .test_support()
                            .p_2()
                            .rounded_md()
                            .bg(cx.theme().muted)
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_sm()
                            .max_h(px(320.))
                            .overflow_y_scroll()
                            .child(text.clone()),
                    ),
            )
            .footer(
                DialogFooter::new()
                    .child(
                        Button::new("changes-cancel")
                            .label(t("button-cancel"))
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("changes-apply")
                            .primary()
                            .label(t("button-apply"))
                            .on_click(apply),
                    ),
            )
    });
}
