//! Services panel (plan 2.7): data sources with their sessions, the JDBC
//! bridge process, and running queries with cancellation.

use std::time::Duration;

use ferrodb_api::{BridgeState, ConnectionState, DataSourceInfo, DriverEvent, SessionInfo};
use ferrodb_ui_kit::{
    gpui::{prelude::*, *},
    icons,
    kit::{
        ActiveTheme, Sizable as _,
        button::{Button, ButtonVariants as _},
        dock::{BasePanel, Panel},
        h_flex, v_flex,
    },
};

use super::{history::time_of_day, panel_boilerplate};
use crate::{
    AppState, activity,
    dialogs::driver_manager::bridge_text,
    i18n::{t, t_args},
};

pub const NAME: &str = "Services";

pub struct ServicesPanel {
    focus: FocusHandle,
    sources: Vec<DataSourceInfo>,
    pub sessions: Vec<SessionInfo>,
    pub bridge: BridgeState,
    _sub: Option<Subscription>,
    _events: Vec<Task<()>>,
}

impl ServicesPanel {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        let backend = AppState::backend(cx);
        let ds_rx = backend.data_sources().subscribe();
        let drv_rx = backend.drivers().subscribe();
        let events = vec![
            cx.spawn(async move |this, cx| {
                while ds_rx.recv().await.is_ok() {
                    if this.update(cx, |this, cx| this.reload(cx)).is_err() {
                        break;
                    }
                }
            }),
            cx.spawn(async move |this, cx| {
                while let Ok(ev) = drv_rx.recv().await {
                    let ok = this.update(cx, |this, cx| {
                        if let DriverEvent::BridgeChanged(b) = ev {
                            this.bridge = b;
                            cx.notify();
                        }
                    });
                    if ok.is_err() {
                        break;
                    }
                }
            }),
            // Sessions have no change events: refresh periodically.
            cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(Duration::from_secs(2)).await;
                    if this.update(cx, |this, cx| this.reload(cx)).is_err() {
                        break;
                    }
                }
            }),
        ];
        let sub = activity::entity(cx).map(|e| cx.observe(&e, |this, _, cx| this.reload(cx)));
        let mut this = Self {
            focus: cx.focus_handle(),
            sources: Vec::new(),
            sessions: Vec::new(),
            bridge: backend.drivers().jdbc_bridge(),
            _sub: sub,
            _events: events,
        };
        this.reload(cx);
        this
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let backend = AppState::backend(cx);
        self.sources = backend.data_sources().list();
        self.sessions = backend.queries().sessions();
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
        let theme = cx.theme().clone();
        let running = activity::entity(cx)
            .map(|e| e.read(cx).running.clone())
            .unwrap_or_default();
        let mut rows: Vec<AnyElement> = Vec::new();
        for s in &self.sources {
            let color = match s.state {
                ConnectionState::Connected => theme.success,
                ConnectionState::Failed(_) => theme.danger,
                ConnectionState::Connecting => theme.warning,
                ConnectionState::Disconnected => theme.muted_foreground,
            };
            rows.push(
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
                    .into_any_element(),
            );
            for sess in self
                .sessions
                .iter()
                .filter(|x| x.data_source == s.config.id)
            {
                rows.push(
                    h_flex()
                        .id(SharedString::from(format!("session-{}", sess.id.0)))
                        .test_support()
                        .gap_2()
                        .pl(px(36.))
                        .py_0p5()
                        .text_color(theme.muted_foreground)
                        .child(icons::console_icon().xsmall())
                        .child(t_args("services-session", &[("id", sess.id.0.into())]))
                        .child(time_of_day(sess.opened_at))
                        .when(sess.in_transaction, |d| {
                            d.child(t("services-in-transaction"))
                        })
                        .into_any_element(),
                );
            }
            for q in running.iter().filter(|q| q.data_source == s.config.id) {
                let id = q.id;
                rows.push(
                    h_flex()
                        .id(SharedString::from(format!("running-{}", q.id.0)))
                        .test_support()
                        .gap_2()
                        .pl(px(36.))
                        .py_0p5()
                        .child(
                            icons::Icon::from(icons::Lucide::Play)
                                .xsmall()
                                .text_color(theme.warning),
                        )
                        .child(
                            div()
                                .flex_1()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .font_family(theme.mono_font_family.clone())
                                .child(q.text.lines().next().unwrap_or_default().to_owned()),
                        )
                        .child(
                            div()
                                .text_color(theme.muted_foreground)
                                .child(format!("{:.1} s", q.started.elapsed().as_secs_f32())),
                        )
                        .child(
                            Button::new(SharedString::from(format!("cancel-{}", q.id.0)))
                                .ghost()
                                .xsmall()
                                .icon(icons::Lucide::Square)
                                .tooltip(t("action-stop"))
                                .on_click(move |_, _, cx| activity::cancel(id, cx)),
                        )
                        .into_any_element(),
                );
            }
        }
        v_flex()
            .id("services")
            .test_support()
            .track_focus(&self.focus)
            .size_full()
            .overflow_y_scroll()
            .text_sm()
            .py_1()
            .child(
                h_flex()
                    .gap_2()
                    .px_2()
                    .pb_1()
                    .child(
                        icons::Icon::from(icons::Lucide::Cpu)
                            .small()
                            .text_color(theme.muted_foreground),
                    )
                    .child(t("bridge-title"))
                    .child(
                        div()
                            .id("services-bridge")
                            .test_support()
                            .text_color(theme.muted_foreground)
                            .child(bridge_text(&self.bridge)),
                    ),
            )
            .children(rows)
    }
}
