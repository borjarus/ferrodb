use ferrodb_ui_kit::{
    gpui::{prelude::*, *},
    kit::{
        ActiveTheme,
        dock::{BasePanel, Panel},
        v_flex,
    },
};

use super::panel_boilerplate;
use crate::i18n::t;

pub const NAME: &str = "Inspector";

/// Value / properties panel (filled in phase 2 by the grid and documents).
pub struct InspectorPanel {
    focus: FocusHandle,
}

impl InspectorPanel {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            focus: cx.focus_handle(),
        }
    }
}

panel_boilerplate!(InspectorPanel);

impl BasePanel for InspectorPanel {
    fn panel_name(&self) -> &'static str {
        NAME
    }
}

impl Panel for InspectorPanel {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        t("panel-inspector")
    }
}

impl Render for InspectorPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("inspector")
            .test_support()
            .track_focus(&self.focus)
            .size_full()
            .items_center()
            .justify_center()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(t("inspector-empty"))
    }
}
