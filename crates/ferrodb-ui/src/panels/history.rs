//! Query history panel (plan 2.7): executed statements from the backend's
//! `HistoryService`, filterable by text and data source; double-click opens
//! the statement in a console.

use std::time::SystemTime;

use ferrodb_api::{HistoryEntry, HistoryFilter};
use ferrodb_ui_kit::{
    gpui::{prelude::*, *},
    icons,
    kit::{
        ActiveTheme, Sizable as _,
        button::{Button, ButtonVariants as _},
        dock::{BasePanel, Panel},
        h_flex,
        input::{Input, InputEvent, InputState},
        v_flex,
    },
};

use super::panel_boilerplate;
use crate::{AppState, i18n::t, workspace};

pub const NAME: &str = "QueryHistory";

/// `14:03:12` (UTC wall clock of the entry).
pub fn time_of_day(at: SystemTime) -> String {
    let secs = at
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let s = secs % 86_400;
    format!("{:02}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
}

pub struct HistoryPanel {
    focus: FocusHandle,
    filter: Entity<InputState>,
    pub entries: Vec<HistoryEntry>,
    _subs: Vec<Subscription>,
    _poll: Task<()>,
}

impl HistoryPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder(t("history-filter")));
        let subs = vec![cx.subscribe(&filter, |this, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change) {
                this.reload(cx);
            }
        })];
        // History grows as consoles run queries; refresh while visible.
        let poll = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(2))
                    .await;
                if this.update(cx, |this, cx| this.reload(cx)).is_err() {
                    break;
                }
            }
        });
        let mut this = Self {
            focus: cx.focus_handle(),
            filter,
            entries: Vec::new(),
            _subs: subs,
            _poll: poll,
        };
        this.reload(cx);
        this
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let text = self.filter.read(cx).value().trim().to_string();
        let backend = AppState::backend(cx);
        let filter = HistoryFilter {
            data_source: None,
            text: (!text.is_empty()).then_some(text),
            limit: Some(500),
        };
        let task = cx
            .background_executor()
            .spawn(async move { backend.history().query(filter).await });
        cx.spawn(async move |this, cx| {
            if let Ok(entries) = task.await {
                let _ = this.update(cx, |this, cx| {
                    if this.entries != entries {
                        this.entries = entries;
                        cx.notify();
                    }
                });
            }
        })
        .detach();
    }
}

panel_boilerplate!(HistoryPanel);

impl BasePanel for HistoryPanel {
    fn panel_name(&self) -> &'static str {
        NAME
    }
}

impl Panel for HistoryPanel {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        t("panel-history")
    }
}

impl Render for HistoryPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let sources = AppState::backend(cx).data_sources().list();
        let rows = self.entries.iter().enumerate().map(|(i, e)| {
            let name = sources
                .iter()
                .find(|s| s.config.id == e.data_source)
                .map(|s| s.config.name.clone())
                .unwrap_or_else(|| e.data_source.to_string());
            let (ds, text) = (e.data_source.clone(), e.text.clone());
            h_flex()
                .id(("history", i))
                .gap_2()
                .px_2()
                .py_0p5()
                .hover(|s| s.bg(theme.muted))
                .on_click(move |ev, window, cx| {
                    if ev.click_count() == 2 {
                        workspace::open_console(Some(ds.clone()), text.clone(), window, cx)
                    }
                })
                .child(
                    icons::Icon::from(if e.succeeded {
                        icons::Lucide::Check
                    } else {
                        icons::Lucide::X
                    })
                    .xsmall()
                    .text_color(if e.succeeded {
                        theme.success
                    } else {
                        theme.danger
                    }),
                )
                .child(
                    div()
                        .w(px(64.))
                        .text_color(theme.muted_foreground)
                        .child(time_of_day(e.executed_at)),
                )
                .child(div().w(px(130.)).overflow_hidden().child(name))
                .child(
                    div()
                        .flex_1()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .font_family(theme.mono_font_family.clone())
                        .child(e.text.lines().next().unwrap_or_default().to_owned()),
                )
                .child(
                    div()
                        .text_color(theme.muted_foreground)
                        .child(format!("{} ms", e.elapsed.as_millis())),
                )
        });
        v_flex()
            .id("query-history")
            .test_support()
            .track_focus(&self.focus)
            .size_full()
            .text_sm()
            .child(
                h_flex()
                    .gap_1()
                    .p_1()
                    .child(div().flex_1().child(Input::new(&self.filter).xsmall()))
                    .child(
                        Button::new("history-refresh")
                            .ghost()
                            .xsmall()
                            .icon(icons::Lucide::RefreshCw)
                            .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
                    ),
            )
            .child(
                div()
                    .id("history-rows")
                    .flex_1()
                    .overflow_y_scroll()
                    .children(rows),
            )
    }
}
