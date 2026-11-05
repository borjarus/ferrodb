use ferrodb_api::{ConnectionState, DataSourceInfo};
use ferrodb_ui_kit::{
    gpui::{prelude::*, *},
    icons,
    kit::{
        ActiveTheme,
        dock::{BasePanel, Panel},
        h_flex, v_flex,
    },
};

use super::panel_boilerplate;
use crate::{AppState, i18n::t};

pub const NAME: &str = "Services";

/// Sessions and running queries (phase 1: connection states of data sources).
pub struct ServicesPanel {
    focus: FocusHandle,
    sources: Vec<DataSourceInfo>,
    _events: Task<()>,
}

impl ServicesPanel {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        let backend = AppState::backend(cx);
        let rx = backend.data_sources().subscribe();
        let events = cx.spawn(async move |this, cx| {
            while rx.recv().await.is_ok() {
                if this.update(cx, |this, cx| this.reload(cx)).is_err() {
                    break;
                }
            }
        });
        let mut this = Self {
            focus: cx.focus_handle(),
            sources: Vec::new(),
            _events: events,
        };
        this.reload(cx);
        this
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        self.sources = AppState::backend(cx).data_sources().list();
        cx.notify();
    }
}

panel_boilerplate!(ServicesPanel);

impl BasePanel for ServicesPanel {
    fn panel_name(&self) -> &'static str {
        NAME
    }
}

impl Panel for ServicesPanel {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        t("panel-services")
    }
}

pub fn state_label(state: &ConnectionState) -> String {
    match state {
        ConnectionState::Disconnected => t("state-disconnected"),
        ConnectionState::Connecting => t("state-connecting"),
        ConnectionState::Connected => t("state-connected"),
        ConnectionState::Failed(e) => format!("{}: {e}", t("state-failed")),
    }
}

impl Render for ServicesPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let rows = self.sources.iter().map(|s| {
            let color = match s.state {
                ConnectionState::Connected => theme.success,
                ConnectionState::Failed(_) => theme.danger,
                ConnectionState::Connecting => theme.warning,
                ConnectionState::Disconnected => theme.muted_foreground,
            };
            h_flex()
                .id(SharedString::from(format!("service-{}", s.config.id)))
                .gap_2()
                .px_2()
                .py_0p5()
                .child(
                    icons::data_source_icon(s.capabilities.data_model)
                        .text_color(theme.muted_foreground),
                )
                .child(div().w(px(180.)).child(s.config.name.clone()))
                .child(div().size(px(8.)).rounded_full().bg(color))
                .child(
                    div()
                        .text_color(theme.muted_foreground)
                        .child(state_label(&s.state)),
                )
        });
        v_flex()
            .id("services")
            .test_support()
            .track_focus(&self.focus)
            .size_full()
            .overflow_y_scroll()
            .text_sm()
            .py_1()
            .children(rows)
    }
}
