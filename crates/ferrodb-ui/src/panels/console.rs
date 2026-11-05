//! Query console. Phase 1: editor with highlighting for the source language,
//! execution against the backend with a summary in the Output panel (result
//! views arrive in phase 2).

use std::time::Instant;

use ferrodb_api::{CancelHandle, DataSourceId, ExecOptions, QueryEvent, QueryLanguage, Script};
use ferrodb_ui_kit::{
    gpui::{prelude::*, *},
    icons,
    kit::{
        ActiveTheme, Disableable as _, Sizable as _, WindowExt as _,
        button::{Button, ButtonVariants as _},
        dock::{BasePanel, Panel, PanelInfo, PanelState},
        h_flex,
        input::{Editor, EditorState},
        notification::Notification,
        v_flex,
    },
    theme::environment_color,
};
use serde::{Deserialize, Serialize};

use super::panel_boilerplate;
use crate::{
    AppState,
    actions::{self, CONSOLE_CONTEXT},
    i18n::{t, t_args},
    output::{self, Level},
};

pub const NAME: &str = "Console";

#[derive(Default, Serialize, Deserialize)]
struct SavedConsole {
    data_source: Option<DataSourceId>,
    text: String,
}

pub struct ConsolePanel {
    focus: FocusHandle,
    editor: Entity<EditorState>,
    data_source: Option<DataSourceId>,
    running: Option<CancelHandle>,
}

/// Editor language id (tree-sitter grammar) for a query language.
pub fn editor_language(lang: Option<&QueryLanguage>) -> &'static str {
    match lang {
        Some(QueryLanguage::Mql) => "javascript",
        _ => "sql",
    }
}

impl ConsolePanel {
    pub fn new(
        data_source: Option<DataSourceId>,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let lang = data_source
            .as_ref()
            .and_then(|id| AppState::backend(cx).data_sources().get(id))
            .map(|s| s.capabilities.language);
        let line_numbers = crate::settings::SettingsStore::get(cx).show_line_numbers;
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language(editor_language(lang.as_ref()))
                .default_value(text)
                .line_number(line_numbers)
        });
        Self {
            focus: cx.focus_handle(),
            editor,
            data_source,
            running: None,
        }
    }

    pub fn restore(state: &PanelState, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let saved: SavedConsole = match &state.info {
            PanelInfo::Panel(v) => serde_json::from_value(v.clone()).unwrap_or_default(),
            _ => SavedConsole::default(),
        };
        Self::new(saved.data_source, saved.text, window, cx)
    }

    pub fn editor(&self) -> &Entity<EditorState> {
        &self.editor
    }

    fn source_name(&self, cx: &App) -> Option<String> {
        let id = self.data_source.as_ref()?;
        AppState::backend(cx)
            .data_sources()
            .get(id)
            .map(|s| s.config.name)
    }

    fn execute(
        &mut self,
        _: &actions::ExecuteStatement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ds) = self.data_source.clone() else {
            window.push_notification(Notification::warning(t("console-no-data-source")), cx);
            return;
        };
        if self.running.is_some() {
            return;
        }
        let backend = AppState::backend(cx);
        let text = self.editor.read(cx).value().to_string();
        let cancel = CancelHandle::new();
        self.running = Some(cancel.clone());
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let started = Instant::now();
            let queries = backend.queries();
            let summary: Result<Vec<String>, String> = async {
                let session = queries.open_session(&ds).await.map_err(|e| e.to_string())?;
                let handle = queries.execute(session, Script::new(text), ExecOptions::default());
                let mut lines = Vec::new();
                let mut items = 0usize;
                while let Ok(ev) = handle.events.recv().await {
                    if cancel.is_cancelled() {
                        handle.cancel.cancel();
                    }
                    match ev {
                        QueryEvent::Data { chunk, .. } => items += chunk.len(),
                        QueryEvent::ResultPaused { has_more, .. } => {
                            lines.push(t_args(
                                "console-result-summary",
                                &[
                                    ("count", items.into()),
                                    ("more", if has_more { "yes" } else { "no" }.into()),
                                ],
                            ));
                            items = 0;
                        }
                        QueryEvent::StatementFinished {
                            affected: Some(n), ..
                        } => {
                            lines.push(t_args("console-affected", &[("count", n.into())]));
                        }
                        QueryEvent::Error(e) => return Err(e.to_string()),
                        QueryEvent::Finished => break,
                        _ => {}
                    }
                }
                let _ = queries.close_session(session).await;
                Ok(lines)
            }
            .await;
            let elapsed = started.elapsed().as_millis() as u64;
            let _ = this.update_in(cx, |this, window, cx| {
                this.running = None;
                match summary {
                    Ok(lines) => {
                        for l in lines {
                            output::info(l, cx);
                        }
                        output::info(t_args("console-finished", &[("ms", elapsed.into())]), cx);
                    }
                    Err(e) => {
                        output::log(Level::Error, e.clone(), cx);
                        window.push_notification(Notification::error(e), cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn stop(&mut self, _: &actions::StopQuery, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(c) = &self.running {
            c.cancel();
            output::log(Level::Warning, t("console-cancelled"), cx);
        }
    }

    fn not_yet(&mut self, phase: u8, window: &mut Window, cx: &mut Context<Self>) {
        window.push_notification(
            Notification::info(t_args("not-implemented-phase", &[("phase", phase.into())])),
            cx,
        );
    }
}

panel_boilerplate!(ConsolePanel);

impl BasePanel for ConsolePanel {
    fn panel_name(&self) -> &'static str {
        NAME
    }

    fn dump(&self, cx: &App) -> PanelState {
        let mut s = PanelState::new(NAME);
        let saved = SavedConsole {
            data_source: self.data_source.clone(),
            text: self.editor.read(cx).value().to_string(),
        };
        s.info = PanelInfo::panel(serde_json::to_value(saved).unwrap_or_default());
        s
    }
}

impl Panel for ConsolePanel {
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let name = self.source_name(cx).unwrap_or_else(|| t("panel-console"));
        h_flex()
            .gap_1()
            .child(icons::console_icon().small())
            .child(name)
    }
}

impl Render for ConsolePanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let source = self
            .data_source
            .as_ref()
            .and_then(|id| AppState::backend(cx).data_sources().get(id));
        let marker = source
            .as_ref()
            .and_then(|s| environment_color(s.config.environment, cx));
        let running = self.running.is_some();
        let header = h_flex()
            .gap_1()
            .px_1()
            .py_0p5()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                Button::new("console-run")
                    .ghost()
                    .xsmall()
                    .icon(icons::Lucide::Play)
                    .loading(running)
                    .tooltip(t("action-execute"))
                    .on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(actions::ExecuteStatement), cx)
                    }),
            )
            .child(
                Button::new("console-stop")
                    .ghost()
                    .xsmall()
                    .icon(icons::Lucide::Square)
                    .disabled(!running)
                    .tooltip(t("action-stop"))
                    .on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(actions::StopQuery), cx)
                    }),
            )
            .child(div().flex_1())
            .when_some(marker, |d, c| {
                d.child(div().size(px(8.)).rounded_full().bg(c))
            })
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(match &source {
                        Some(s) => format!(
                            "{} · {}",
                            s.config.name,
                            s.capabilities.language.display_name()
                        ),
                        None => t("console-no-data-source"),
                    }),
            );
        v_flex()
            .id("console")
            .test_support()
            .key_context(CONSOLE_CONTEXT)
            .track_focus(&self.focus)
            .size_full()
            .on_action(cx.listener(Self::execute))
            .on_action(cx.listener(Self::stop))
            .on_action(cx.listener(|this, _: &actions::FormatCode, w, cx| this.not_yet(5, w, cx)))
            .on_action(cx.listener(|this, _: &actions::Rename, w, cx| this.not_yet(5, w, cx)))
            .on_action(
                cx.listener(|this, _: &actions::GoToDeclaration, w, cx| this.not_yet(4, w, cx)),
            )
            .on_action(
                cx.listener(|this, _: &actions::LiveTemplates, w, cx| this.not_yet(5, w, cx)),
            )
            .on_action(
                cx.listener(|this, _: &actions::QuickDocumentation, w, cx| this.not_yet(4, w, cx)),
            )
            .child(header)
            .child(div().flex_1().child(Editor::new(&self.editor).size_full()))
    }
}
