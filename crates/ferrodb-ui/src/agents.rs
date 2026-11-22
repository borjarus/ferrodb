//! External agents (phase 9): the consent dialog for tool calls coming over
//! MCP and the state shown in the status bar and the Agents settings page.
//!
//! The workspace subscribes to `AgentService::subscribe` once; events land
//! in the [`AgentsHub`] global on the UI thread. Consent requests queue up
//! and are shown one at a time; a request closed by the backend (timeout,
//! client gone) closes its dialog.

use std::collections::VecDeque;

use ferrodb_api::{AgentEvent, ConsentDecision, ConsentRequest, McpStatus};
use ferrodb_ui_kit::{
    gpui::{prelude::*, *},
    icons,
    kit::{
        ActiveTheme, Disableable as _, Sizable as _, WindowExt as _,
        button::{Button, ButtonVariants as _},
        dialog::DialogFooter,
        h_flex,
        input::{Editor, EditorState},
        tag::Tag,
        v_flex,
    },
};

use crate::{
    AppState,
    i18n::{t, t_args},
    output,
};

/// Bumped when the AI settings are saved (views showing the assistant's
/// state re-render).
#[derive(Clone, Copy, Default)]
pub struct AiSettingsRevision(pub u64);
impl Global for AiSettingsRevision {}

pub fn bump_ai_settings(cx: &mut App) {
    let n = cx.try_global::<AiSettingsRevision>().map_or(0, |r| r.0);
    cx.set_global(AiSettingsRevision(n + 1));
}

/// Agent state of the application.
#[derive(Default)]
pub struct AgentsHub {
    pub status: McpStatus,
    /// Requests waiting behind the one shown.
    pub queue: VecDeque<ConsentRequest>,
    /// The request whose dialog is open.
    pub showing: Option<ConsentRequest>,
    /// Bumped on every audit entry (the settings page reloads the log).
    pub audit_rev: u64,
}
impl Global for AgentsHub {}

pub fn init(cx: &mut App) {
    cx.set_global(AiSettingsRevision::default());
    let status = AppState::backend(cx).agents().status();
    cx.set_global(AgentsHub {
        status,
        ..Default::default()
    });
}

pub fn hub(cx: &App) -> Option<&AgentsHub> {
    cx.try_global::<AgentsHub>()
}

fn update_hub(cx: &mut App, f: impl FnOnce(&mut AgentsHub)) {
    if cx.try_global::<AgentsHub>().is_none() {
        cx.set_global(AgentsHub::default());
    }
    cx.update_global::<AgentsHub, _>(|h, _| f(h));
}

/// Forwards agent events to the UI thread for the life of the returned task.
/// Requests already waiting are shown first.
pub fn listen(window: &mut Window, cx: &mut App) -> Task<()> {
    let backend = AppState::backend(cx);
    if !backend.agents().available() {
        return Task::ready(());
    }
    if cx.try_global::<AgentsHub>().is_none() {
        init(cx);
    }
    let status = backend.agents().status();
    let pending = backend.agents().pending();
    update_hub(cx, |h| {
        h.status = status;
        for r in pending {
            if h.showing.as_ref().is_none_or(|s| s.id != r.id)
                && !h.queue.iter().any(|q| q.id == r.id)
            {
                h.queue.push_back(r);
            }
        }
    });
    let rx = backend.agents().subscribe();
    window.spawn(cx, async move |cx| {
        // Not while the window is being built: dialogs need its root.
        if cx.update(show_next).is_err() {
            return;
        }
        while let Ok(ev) = rx.recv().await {
            if cx.update(|window, cx| handle(ev, window, cx)).is_err() {
                break;
            }
        }
    })
}

/// Applies one agent event.
pub fn handle(ev: AgentEvent, window: &mut Window, cx: &mut App) {
    match ev {
        AgentEvent::ConsentRequested(r) => {
            update_hub(cx, |h| {
                if h.showing.as_ref().is_none_or(|s| s.id != r.id)
                    && !h.queue.iter().any(|q| q.id == r.id)
                {
                    h.queue.push_back(r);
                }
            });
            show_next(window, cx);
        }
        AgentEvent::ConsentClosed { id } => {
            let shown = hub(cx)
                .and_then(|h| h.showing.as_ref())
                .is_some_and(|s| s.id == id);
            update_hub(cx, |h| {
                h.queue.retain(|q| q.id != id);
                if shown {
                    h.showing = None;
                }
            });
            if shown {
                window.close_dialog(cx);
                output::info(t("consent-closed"), cx);
                show_next(window, cx);
            }
        }
        AgentEvent::Audit(_) => update_hub(cx, |h| h.audit_rev += 1),
        AgentEvent::Status(s) => update_hub(cx, |h| h.status = s),
    }
    cx.refresh_windows();
}

/// Opens the dialog of the next queued request, if none is open.
fn show_next(window: &mut Window, cx: &mut App) {
    let Some(next) = hub(cx).and_then(|h| {
        if h.showing.is_some() {
            None
        } else {
            h.queue.front().cloned()
        }
    }) else {
        return;
    };
    update_hub(cx, |h| {
        h.queue.pop_front();
        h.showing = Some(next.clone());
    });
    open_dialog(next, window, cx);
}

/// Answers the request shown in the dialog and closes it.
pub fn decide(decision: ConsentDecision, window: &mut Window, cx: &mut App) {
    if record(decision, cx) {
        window.close_dialog(cx);
        show_next(window, cx);
    }
}

/// Sends the decision of the shown request to the backend; false when no
/// request is shown.
fn record(decision: ConsentDecision, cx: &mut App) -> bool {
    let Some(r) = hub(cx).and_then(|h| h.showing.clone()) else {
        return false;
    };
    // Sessions are never granted for calls that modify data.
    let decision = match decision {
        ConsentDecision::AllowForSession if r.modifies => ConsentDecision::AllowOnce,
        d => d,
    };
    update_hub(cx, |h| h.showing = None);
    AppState::backend(cx).agents().decide(r.id, decision);
    let label = match decision {
        ConsentDecision::AllowOnce | ConsentDecision::AllowForSession => "consent-allowed",
        ConsentDecision::Deny => "consent-denied",
    };
    output::info(
        t_args(
            label,
            &[("client", r.client.into()), ("tool", r.tool.into())],
        ),
        cx,
    );
    true
}

fn open_dialog(r: ConsentRequest, window: &mut Window, cx: &mut App) {
    let text = r.text.clone().unwrap_or_default();
    let editor = (!text.is_empty()).then(|| {
        cx.new(|cx| {
            EditorState::new(window, cx)
                .language("sql")
                .default_value(text)
                .line_number(false)
        })
    });
    window.open_dialog(cx, move |dialog, _, cx| {
        let theme = cx.theme().clone();
        let r = r.clone();
        let row = |label: String, value: String| {
            h_flex()
                .gap_2()
                .text_sm()
                .child(
                    div()
                        .w(px(110.))
                        .text_color(theme.muted_foreground)
                        .child(label),
                )
                .child(div().flex_1().child(value))
        };
        let source = r
            .data_source_name
            .clone()
            .or_else(|| r.data_source.as_ref().map(|d| d.to_string()))
            .unwrap_or_else(|| "—".into());
        dialog
            .title(t("consent-title"))
            .w(px(600.))
            .keyboard(true)
            .overlay_closable(false)
            // Escape denies.
            .on_cancel(|_, window, cx| {
                record(ConsentDecision::Deny, cx);
                // After this dialog has closed.
                window.defer(cx, show_next);
                true
            })
            .child(
                v_flex()
                    .id("consent-dialog")
                    .test_support()
                    .gap_2()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                icons::Icon::from(icons::Lucide::ShieldAlert)
                                    .text_color(theme.warning),
                            )
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(r.summary.clone()),
                            ),
                    )
                    .child(row(t("consent-client"), r.client.clone()))
                    .child(row(t("consent-tool"), r.tool.clone()))
                    .child(row(t("consent-source"), source))
                    .when(r.modifies || !r.risks.is_empty(), |d| {
                        d.child(
                            v_flex()
                                .id("consent-modifies")
                                .test_support()
                                .gap_1()
                                .p_2()
                                .rounded_md()
                                .bg(theme.danger.opacity(0.12))
                                .text_color(theme.danger)
                                .text_sm()
                                .child(t("consent-modifies"))
                                .child(h_flex().gap_1().flex_wrap().children(r.risks.iter().map(
                                    |k| {
                                        Tag::danger()
                                            .xsmall()
                                            .child(crate::panels::console::risk_label(*k))
                                    },
                                ))),
                        )
                    })
                    .when_some(editor.clone(), |d, e| {
                        d.child(
                            div()
                                .h(px(160.))
                                .border_1()
                                .border_color(theme.border)
                                .rounded_md()
                                .child(Editor::new(&e).size_full().readonly(true)),
                        )
                    }),
            )
            .footer(
                DialogFooter::new()
                    .child(
                        Button::new("consent-deny")
                            .label(t("consent-deny"))
                            .on_click(|_, window, cx| decide(ConsentDecision::Deny, window, cx)),
                    )
                    .child(
                        Button::new("consent-allow-session")
                            .label(t("consent-allow-session"))
                            .disabled(r.modifies)
                            .on_click(|_, window, cx| {
                                decide(ConsentDecision::AllowForSession, window, cx)
                            }),
                    )
                    .child(
                        Button::new("consent-allow-once")
                            .primary()
                            .when(r.modifies, |b| b.danger())
                            .label(t("consent-allow-once"))
                            .on_click(|_, window, cx| {
                                decide(ConsentDecision::AllowOnce, window, cx)
                            }),
                    ),
            )
    });
}

/// Status-bar text while the MCP server runs.
pub fn status_text(status: &McpStatus) -> Option<String> {
    status
        .running
        .then(|| t_args("status-mcp", &[("clients", status.clients.len().into())]))
}
