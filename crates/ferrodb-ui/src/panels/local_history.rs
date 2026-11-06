//! Local History panel (plan 2.7): snapshots of console texts taken on
//! every execution; compare with the current text or restore into a console.

use ferrodb_ui_kit::{
    gpui::{prelude::*, *},
    icons,
    kit::{
        ActiveTheme, Sizable as _, WindowExt as _,
        button::{Button, ButtonVariants as _},
        dock::{BasePanel, Panel},
        h_flex, v_flex,
    },
};

use super::{history::time_of_day, panel_boilerplate};
use crate::{AppState, activity, i18n::t, views::diff::DiffView, workspace};

pub const NAME: &str = "LocalHistory";

pub struct LocalHistoryPanel {
    focus: FocusHandle,
    selected: Option<usize>,
    _sub: Option<Subscription>,
}

impl LocalHistoryPanel {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        let sub = activity::entity(cx).map(|e| cx.observe(&e, |_, _, cx| cx.notify()));
        Self {
            focus: cx.focus_handle(),
            selected: None,
            _sub: sub,
        }
    }

    fn compare(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(snap) = activity::entity(cx).and_then(|e| e.read(cx).snapshots.get(ix).cloned())
        else {
            return;
        };
        let current = workspace::active_console_text(cx).unwrap_or_default();
        let view = cx.new(|cx| {
            DiffView::new(
                format!("{} {}", t("local-history-snapshot"), time_of_day(snap.at)),
                &snap.text,
                t("local-history-current"),
                &current,
                "sql",
                cx,
            )
        });
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title(t("action-local-history"))
                .w(px(900.))
                .child(div().h(px(480.)).child(view.clone()))
        });
    }
}

panel_boilerplate!(LocalHistoryPanel);

impl BasePanel for LocalHistoryPanel {
    fn panel_name(&self) -> &'static str {
        NAME
    }
}

impl Panel for LocalHistoryPanel {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        t("panel-local-history")
    }
}

impl Render for LocalHistoryPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let snapshots = activity::entity(cx)
            .map(|e| e.read(cx).snapshots.clone())
            .unwrap_or_default();
        let sources = AppState::backend(cx).data_sources().list();
        let rows = snapshots.iter().enumerate().rev().map(|(i, s)| {
            let name = s
                .data_source
                .as_ref()
                .and_then(|d| sources.iter().find(|x| &x.config.id == d))
                .map(|x| x.config.name.clone())
                .unwrap_or_default();
            let selected = self.selected == Some(i);
            let (ds, text) = (s.data_source.clone(), s.text.clone());
            h_flex()
                .id(("local-history", i))
                .gap_2()
                .px_2()
                .py_0p5()
                .when(selected, |d| d.bg(theme.accent))
                .hover(|s| s.bg(theme.muted))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.selected = Some(i);
                    cx.notify();
                }))
                .child(
                    div()
                        .w(px(64.))
                        .text_color(theme.muted_foreground)
                        .child(time_of_day(s.at)),
                )
                .child(
                    div()
                        .w(px(110.))
                        .text_color(theme.muted_foreground)
                        .child(t(s.reason)),
                )
                .child(div().w(px(130.)).overflow_hidden().child(name))
                .child(
                    div()
                        .flex_1()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .font_family(theme.mono_font_family.clone())
                        .child(s.text.lines().next().unwrap_or_default().to_owned()),
                )
                .child(
                    Button::new(("lh-compare", i))
                        .ghost()
                        .xsmall()
                        .icon(icons::Lucide::GitCompare)
                        .tooltip(t("local-history-compare"))
                        .on_click(cx.listener(move |this, _, w, cx| this.compare(i, w, cx))),
                )
                .child(
                    Button::new(("lh-restore", i))
                        .ghost()
                        .xsmall()
                        .icon(icons::Lucide::Undo2)
                        .tooltip(t("local-history-restore"))
                        .on_click(move |_, window, cx| {
                            workspace::open_console(ds.clone(), text.clone(), window, cx)
                        }),
                )
        });
        v_flex()
            .id("local-history")
            .test_support()
            .track_focus(&self.focus)
            .size_full()
            .text_sm()
            .when(snapshots.is_empty(), |d| {
                d.child(
                    div()
                        .p_2()
                        .text_color(theme.muted_foreground)
                        .child(t("local-history-empty")),
                )
            })
            .child(
                div()
                    .id("lh-rows")
                    .flex_1()
                    .overflow_y_scroll()
                    .children(rows),
            )
    }
}
